//! Probe: ffprobe wrapper + pure accept/reject decision + magic-byte sniffing.
//!
//! Ordering is probe-before-transcode (MANDATORY per ARCHITECTURE.md §5):
//! duration is unknowable from magic bytes alone.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;

use crate::error::MediaError;

/// Default clip length cap (admin-adjustable via settings `clip_max_seconds`).
pub const CLIP_MAX_SECONDS_DEFAULT: f64 = 180.0;

/// Hard cap on a single ffprobe run; a straggler is killed and surfaced as
/// `MediaError::ProbeTimeout`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Container kind sniffed from leading bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Container {
    pub ext: &'static str,
    pub mime: &'static str,
}

pub const MP4: Container = Container {
    ext: "mp4",
    mime: "video/mp4",
};
pub const MOV: Container = Container {
    ext: "mov",
    mime: "video/quicktime",
};
pub const WEBM: Container = Container {
    ext: "webm",
    mime: "video/webm",
};

/// Metadata parsed from `ffprobe -json -show_format -show_streams`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProbeInfo {
    pub duration_s: Option<f64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub has_audio: bool,
}

/// Pure accept/reject logic for the REJECT path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeDecision {
    Accept,
    Reject(&'static str),
}

/// Decide whether a probed file may proceed. Over-cap duration or undecodable
/// (no duration / non-positive duration) rejects; the caller turns that into
/// `clip.status = 'failed'` + file cleanup + an uploader-facing reason.
pub fn decide(info: &ProbeInfo, cap_seconds: f64) -> ProbeDecision {
    match info.duration_s {
        None => ProbeDecision::Reject("undecodable: no duration reported by ffprobe"),
        Some(d) if !d.is_finite() || d <= 0.0 => {
            ProbeDecision::Reject("undecodable: invalid duration reported by ffprobe")
        }
        Some(d) if d > cap_seconds => {
            ProbeDecision::Reject("duration exceeds clip_max_seconds cap")
        }
        Some(_) => ProbeDecision::Accept,
    }
}

/// Sniff container type from magic bytes: mp4/mov `ftyp` at offset 4,
/// webm EBML (`1A 45 DF A3`) at offset 0. Anything else is unsupported.
pub fn sniff_container(data: &[u8]) -> Result<Container, MediaError> {
    if data.len() >= 4 && data[..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return Ok(WEBM);
    }
    if data.len() >= 8 && &data[4..8] == b"ftyp" {
        let brand = if data.len() >= 12 { &data[8..12] } else { b"" };
        return Ok(if brand == b"qt  " { MOV } else { MP4 });
    }
    Err(MediaError::Unsupported)
}

/// Run `ffprobe` against `path` and parse duration / width / height / audio.
///
/// `Err` means the file cannot be decoded (ffprobe missing, nonzero exit, or
/// no video stream), or ffprobe overran [`PROBE_TIMEOUT`]. `Ok` still leaves
/// `duration_s` optional — the [`decide`] step is what rejects
/// missing/non-positive durations.
pub async fn probe(path: &Path) -> Result<ProbeInfo, MediaError> {
    let mut child = tokio::process::Command::new("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_format")
        .arg("-show_streams")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| MediaError::Probe(format!("ffprobe spawn failed: {e}")))?;

    let done = async {
        let status = child
            .wait()
            .await
            .map_err(|e| MediaError::Probe(format!("ffprobe wait failed: {e}")))?;
        let mut stdout = Vec::new();
        if let Some(mut out) = child.stdout.take() {
            tokio::io::AsyncReadExt::read_to_end(&mut out, &mut stdout)
                .await
                .map_err(|e| MediaError::Probe(format!("ffprobe stdout read failed: {e}")))?;
        }
        let mut stderr = Vec::new();
        if let Some(mut err) = child.stderr.take() {
            tokio::io::AsyncReadExt::read_to_end(&mut err, &mut stderr)
                .await
                .map_err(|e| MediaError::Probe(format!("ffprobe stderr read failed: {e}")))?;
        }
        Ok::<_, MediaError>((status, stdout, stderr))
    };

    let (status, stdout, stderr) = match tokio::time::timeout(PROBE_TIMEOUT, done).await {
        Ok(result) => result?,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(MediaError::ProbeTimeout);
        }
    };

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr);
        return Err(MediaError::Undecodable(stderr.trim().to_string()));
    }

    let parsed: FfprobeOutput = serde_json::from_slice(&stdout)
        .map_err(|e| MediaError::Probe(format!("ffprobe JSON parse failed: {e}")))?;

    let duration_s = parsed
        .format
        .duration
        .as_deref()
        .and_then(|d| d.parse::<f64>().ok());

    let video = parsed
        .streams
        .iter()
        .find(|s| s.codec_type.as_deref() == Some("video"));
    let width = video.and_then(|s| s.width).map(|w| w as i32);
    let height = video.and_then(|s| s.height).map(|h| h as i32);
    let has_audio = parsed
        .streams
        .iter()
        .any(|s| s.codec_type.as_deref() == Some("audio"));

    if video.is_none() {
        return Err(MediaError::Undecodable("no video stream found".to_string()));
    }

    Ok(ProbeInfo {
        duration_s,
        width,
        height,
        has_audio,
    })
}

#[derive(Debug, Deserialize)]
struct FfprobeOutput {
    format: FfprobeFormat,
    streams: Vec<FfprobeStream>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    #[serde(default)]
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    #[serde(default)]
    codec_type: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}
