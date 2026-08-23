//! Transcode ladder: H.264+AAC mp4 renditions (only up to source height) plus
//! a poster frame at 25% of the duration.
//!
//! Every mp4 output gets `-movflags +faststart` so `moov` is front-loaded —
//! range-request playback depends on it (ARCHITECTURE.md §5), and the CI
//! fixture test asserts the atom order. Each ffmpeg run is bounded by
//! [`TRANSCODE_TIMEOUT`]; a straggler is killed and surfaced as
//! `MediaError::TranscodeTimeout`.

use std::ffi::OsStr;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::error::MediaError;
use crate::probe::{self, ProbeInfo};

/// Per-rung hard cap. A single clip transcodes each rung independently and
/// each rung is individually bounded by [`TRANSCODE_TIMEOUT`]; the worker
/// additionally wraps the whole ladder (the whole job) in a job-level timeout,
/// so a straggling ladder is eventually killed and surfaced as a failed clip.
pub const TRANSCODE_TIMEOUT: Duration = Duration::from_secs(120);

/// `-threads` value passed to every ffmpeg invocation (default 2; overridable
/// via [`set_threads`] from the server config key `ffmpeg_threads`).
static FFMPEG_THREADS: AtomicU32 = AtomicU32::new(2);

/// Override the `-threads` value used for every ffmpeg run (`0`/`1` are
/// clamped to 1). Tests rely on the default of 2.
pub fn set_threads(n: u32) {
    FFMPEG_THREADS.store(n.max(1), Ordering::Relaxed);
}

/// Poster filename written into the workdir.
pub const POSTER_JPG: &str = "poster.jpg";

/// One produced ladder rung.
#[derive(Debug, Clone)]
pub struct TranscodeVideo {
    pub rendition: &'static str,
    pub path: PathBuf,
    pub size_bytes: u64,
}

/// Everything `transcode` wrote into the workdir.
#[derive(Debug, Clone)]
pub struct TranscodeOutputs {
    pub videos: Vec<TranscodeVideo>,
    pub poster_path: PathBuf,
    /// Probe metadata used to decide the ladder (also stamped by the caller).
    pub info: ProbeInfo,
}

/// Transcode `original` into `workdir`, producing `{720|480}.mp4` rungs plus
/// `poster.jpg`. Rungs are capped by the source height: 720p only when the
/// source is ≥720 tall, while a 480p rung is always produced (sub-480p
/// sources are scaled up) so every served mp4 rung is faststart h264/aac.
pub async fn transcode(original: &Path, workdir: &Path) -> Result<TranscodeOutputs, MediaError> {
    let info = probe::probe(original).await?;
    let height = info.height.unwrap_or(0);

    let mut videos = Vec::new();
    for (rendition, h) in rungs(height) {
        let out = workdir.join(format!("{rendition}.mp4"));
        transcode_mp4(original, &out, h, info.has_audio).await?;
        let size_bytes = tokio::fs::metadata(&out)
            .await
            .map_err(MediaError::Io)?
            .len();
        videos.push(TranscodeVideo {
            rendition,
            path: out,
            size_bytes,
        });
    }

    let poster_path = workdir.join(POSTER_JPG);
    let at_seconds = info.duration_s.unwrap_or(0.0) * 0.25;
    transcode_poster(original, &poster_path, at_seconds).await?;

    Ok(TranscodeOutputs {
        videos,
        poster_path,
        info,
    })
}

/// Ladder rungs allowed for a source of `height` px: 720 requires a ≥720p
/// source; a 480p rung is always produced so every served mp4 rung is a
/// faststart h264/aac rendition. Sub-480p sources are scaled UP to 480p
/// (`scale=-2:480`); the original file is kept as-is under the `orig` row.
fn rungs(height: i32) -> Vec<(&'static str, u32)> {
    let mut out = Vec::new();
    if height >= 720 {
        out.push(("720", 720));
    }
    out.push(("480", 480));
    out
}

/// `ffmpeg <args...>` with a hard [`TRANSCODE_TIMEOUT`]; kills stragglers.
/// Every invocation carries `-threads N` (`N` from [`FFMPEG_THREADS`]).
async fn run_ffmpeg(args: &[&OsStr]) -> Result<(), MediaError> {
    let threads = format!("{}", FFMPEG_THREADS.load(Ordering::Relaxed));
    let mut full_args: Vec<OsString> = Vec::with_capacity(args.len() + 2);
    full_args.push("-threads".into());
    full_args.push(threads.into());
    full_args.extend(args.iter().map(|a| a.to_os_string()));

    let mut child = tokio::process::Command::new("ffmpeg")
        .args(&full_args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| MediaError::Probe(format!("ffmpeg spawn failed: {e}")))?;

    let done = async {
        let status = child.wait().await.map_err(MediaError::Io)?;
        let mut stderr = String::new();
        if let Some(mut err) = child.stderr.take() {
            let _ = tokio::io::AsyncReadExt::read_to_string(&mut err, &mut stderr).await;
        }
        if status.success() {
            Ok(())
        } else {
            Err(MediaError::Transcode(stderr.trim().to_string()))
        }
    };

    match tokio::time::timeout(TRANSCODE_TIMEOUT, done).await {
        Ok(result) => result,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(MediaError::TranscodeTimeout)
        }
    }
}

/// One mp4 rung: H.264 High profile + AAC audio, `-movflags +faststart`.
async fn transcode_mp4(
    original: &Path,
    out: &Path,
    height: u32,
    has_audio: bool,
) -> Result<(), MediaError> {
    let mut args: Vec<OsString> = vec![
        "-y".into(),
        "-v".into(),
        "error".into(),
        "-i".into(),
        original.as_os_str().to_owned(),
        "-c:v".into(),
        "libx264".into(),
        "-profile:v".into(),
        "high".into(),
        "-preset".into(),
        "veryfast".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-vf".into(),
        format!("scale=-2:{height}").into(),
    ];
    if has_audio {
        args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "128k".into()]);
    } else {
        args.push("-an".into());
    }
    args.extend(["-movflags".into(), "+faststart".into()]);
    args.push(out.as_os_str().to_owned());
    run_ffmpeg(&arg_refs(&args)).await
}

/// Poster frame at `at_seconds`, scaled to fit within 720px on the long side.
async fn transcode_poster(original: &Path, out: &Path, at_seconds: f64) -> Result<(), MediaError> {
    let args: Vec<OsString> = vec![
        "-y".into(),
        "-v".into(),
        "error".into(),
        "-ss".into(),
        format!("{at_seconds:.3}").into(),
        "-i".into(),
        original.as_os_str().to_owned(),
        "-vf".into(),
        "scale=720:720:force_original_aspect_ratio=decrease".into(),
        "-frames:v".into(),
        "1".into(),
        "-q:v".into(),
        "2".into(),
        out.as_os_str().to_owned(),
    ];
    run_ffmpeg(&arg_refs(&args)).await
}

fn arg_refs(args: &[OsString]) -> Vec<&OsStr> {
    args.iter().map(|a| a.as_os_str()).collect()
}
