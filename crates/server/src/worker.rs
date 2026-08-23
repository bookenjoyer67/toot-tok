//! Job runner. Spawns `worker_concurrency` detached tasks that claim due jobs
//! with `SKIP LOCKED`, dispatch on `kind`, and poll every 2s when the queue is
//! empty. Handled kinds: `probe` -> `transcode` -> `finalize`. Anything unknown
//! is dead-lettered with `last_error` for admin visibility.
//!
//! A background maintenance loop (same spawned task) reaps stale-lock
//! `running` jobs every 30s and runs the media GC every 60s.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sqlx::PgPool;
use toottok_db::clip::Clip;
use toottok_db::follow::Follow;
use toottok_db::job::Job;
use toottok_db::media_asset::MediaAsset;
use toottok_media::probe;
use toottok_media::probe::ProbeDecision;
use toottok_media::store::Store;
use toottok_media::transcode;
use tracing::{error, info, warn};

use crate::settings::numeric_setting;

/// Default clip length cap in seconds (settings key `clip_max_seconds`).
pub const CLIP_MAX_SECONDS_DEFAULT: f64 = probe::CLIP_MAX_SECONDS_DEFAULT;

const POLL_EMPTY_DELAY: Duration = Duration::from_secs(2);
/// Reaper + GC cadence. Reaping runs every tick; GC every other tick (60s).
const MAINTENANCE_TICK: Duration = Duration::from_secs(30);

/// Spawn `concurrency` (min 1) detached worker tasks and log the pool size.
/// Detached via `tokio::spawn` so dropping the handles does not abort them.
/// `base_url` is the public base URL used to build canonical federation ids
/// when a finalized local clip is announced (Create(Note)).
pub async fn spawn_worker_pool(
    pool: PgPool,
    store: Arc<dyn Store>,
    concurrency: usize,
    job_timeout: Duration,
    egress: toottok_federation::EgressGuard,
    base_url: String,
) {
    let n = concurrency.max(1);
    for i in 0..n {
        let pool = pool.clone();
        let store = store.clone();
        let egress = egress.clone();
        let base_url = base_url.clone();
        tokio::spawn(async move {
            worker_loop(
                &format!("worker-{i}"),
                pool,
                store,
                job_timeout,
                egress,
                base_url,
            )
            .await;
        });
    }
    info!(workers = n, "worker pool up");
}

/// A single detached delivery worker for federation `deliver` / `deliver_follow`
/// jobs (signed, egress-guarded POSTs with backoff). Polls the same queue as
/// the main pool; kept separate so an unbounded federation backlog never crowds
/// out media jobs.
pub async fn spawn_delivery_worker(pool: PgPool, egress: toottok_federation::EgressGuard) {
    tokio::spawn(async move {
        loop {
            match Job::claim_next_due(&pool, "delivery").await {
                Ok(Some(job)) if job.kind.starts_with("deliver") => {
                    process_deliver(&pool, &egress, job).await;
                }
                Ok(Some(job)) => {
                    // Not a delivery job: hand it back untouched so the main
                    // pool (SKIP LOCKED) can claim it.
                    let _ = sqlx::query(
                        "UPDATE jobs SET state='queued', locked_by=NULL, locked_at=NULL WHERE id=$1",
                    )
                    .bind(job.id)
                    .execute(&pool)
                    .await;
                }
                Ok(None) => tokio::time::sleep(POLL_EMPTY_DELAY).await,
                Err(e) => {
                    warn!(error = %e, "delivery worker claim failed; backing off");
                    tokio::time::sleep(POLL_EMPTY_DELAY).await;
                }
            }
        }
    });
}

/// Spawn the maintenance loop: stale-lock reaper every 30s + media GC every
/// 60s, both on the same 30s ticker.
pub async fn spawn_maintenance(pool: PgPool, media_dir: PathBuf, job_timeout_secs: u64) {
    tokio::spawn(async move {
        maintenance_loop(pool, media_dir, job_timeout_secs).await;
    });
}

async fn maintenance_loop(pool: PgPool, media_dir: PathBuf, job_timeout_secs: u64) {
    let mut tick = 0u64;
    loop {
        tokio::time::sleep(MAINTENANCE_TICK).await;
        tick += 1;
        reap_stale_jobs(&pool, job_timeout_secs).await;
        if tick.is_multiple_of(2) {
            media_gc(&pool, &media_dir).await;
        }
    }
}

/// Requeue `running` jobs whose lock is older than `job_timeout_secs`,
/// incrementing `attempts`; jobs at/past `max_attempts` go `dead` and their
/// clip (when the payload carries one) is flipped to `failed`.
async fn reap_stale_jobs(pool: &PgPool, job_timeout_secs: u64) {
    let rows = match sqlx::query_as::<_, Job>(
        r#"
        UPDATE jobs
        SET state = CASE WHEN attempts + 1 >= max_attempts THEN 'dead' ELSE 'queued' END,
            last_error = CASE WHEN attempts + 1 >= max_attempts THEN 'reaper: max attempts' ELSE last_error END,
            attempts = attempts + 1,
            locked_by = NULL,
            locked_at = NULL
        WHERE state = 'running'
          AND locked_at < now() - ($1::bigint * interval '1 second')
        RETURNING *
        "#,
    )
    .bind(job_timeout_secs as i64)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "reaper: stale-job sweep failed");
            return;
        }
    };

    if rows.is_empty() {
        return;
    }
    info!(n = rows.len(), "reaper: requeued stale jobs");
    for job in rows {
        if job.state == "dead" {
            if let Some(clip_id) = job.payload.get("clip_id").and_then(|v| v.as_i64()) {
                if let Err(e) = Clip::mark_failed(pool, clip_id).await {
                    error!(id = job.id, clip_id, error = %e, "reaper: failed to fail clip");
                }
            }
        }
    }
}

/// One `media_assets` row plus whether its owning clip is soft-deleted.
#[derive(sqlx::FromRow)]
struct GcRow {
    id: i64,
    path: String,
    clip_deleted: bool,
}

/// Media GC: delete files under `media_dir` not referenced by
/// `media_assets.path`, delete asset rows whose file is gone, and sweep the
/// files + rows of assets whose clip has `deleted_at` set.
///
/// Unreferenced files younger than [`GC_GRACE_SECS`] are skipped regardless of
/// references: an upload registers its `orig` media_assets row early, but a
/// row that never landed (or a mid-write file) must not be eaten before the
/// pipeline had a chance to reference it.
pub async fn media_gc(pool: &PgPool, media_dir: &Path) {
    let referenced: HashSet<String> =
        match sqlx::query_scalar::<_, String>("SELECT path FROM media_assets")
            .fetch_all(pool)
            .await
        {
            Ok(rows) => rows.into_iter().collect(),
            Err(e) => {
                warn!(error = %e, "media_gc: failed to read referenced asset paths");
                return;
            }
        };

    let rows: Vec<GcRow> = match sqlx::query_as::<_, GcRow>(
        r#"
        SELECT ma.id, ma.path, (c.deleted_at IS NOT NULL) AS clip_deleted
        FROM media_assets ma
        LEFT JOIN clips c ON c.id = ma.clip_id
        "#,
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "media_gc: failed to read asset rows");
            return;
        }
    };

    for row in rows {
        let abs = media_dir.join(&row.path);
        let file_gone = tokio::fs::metadata(&abs).await.is_err();
        if !file_gone && !row.clip_deleted {
            continue;
        }
        if !file_gone {
            // Sweep the file first (deleted-clip path).
            if let Err(e) = tokio::fs::remove_file(&abs).await {
                warn!(path = %row.path, error = %e, "media_gc: failed to remove file");
            }
        }
        if let Err(e) = sqlx::query("DELETE FROM media_assets WHERE id = $1")
            .bind(row.id)
            .execute(pool)
            .await
        {
            warn!(asset_id = row.id, error = %e, "media_gc: failed to delete asset row");
        }
    }

    // Delete files under original/ + renditions/ not referenced by any row.
    // A file written within the grace window is never deleted even when
    // unreferenced — its originating upload may still be in flight.
    for root in ["original", "renditions"] {
        let root = media_dir.join(root);
        let mut files = Vec::new();
        if let Err(e) = walk_files(&root, &mut files).await {
            warn!(root = %root.display(), error = %e, "media_gc: walk failed");
            continue;
        }
        for file in files {
            let Ok(rel) = file.strip_prefix(media_dir) else {
                continue;
            };
            let rel = rel.to_string_lossy().into_owned();
            if !referenced.contains(&rel) && is_stale(&file).await {
                if let Err(e) = tokio::fs::remove_file(&file).await {
                    warn!(path = %rel, error = %e, "media_gc: failed to remove unreferenced file");
                }
            }
        }
    }
}

/// GC grace window: files modified within this are presumed still in flight.
const GC_GRACE_SECS: u64 = 3600;

/// True when `path` is old enough to be swept. Files whose mtime cannot be
/// read or is in the future (clock skew) are treated as young and kept.
async fn is_stale(path: &Path) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(meta) => match meta.modified() {
            Ok(modified) => match modified.elapsed() {
                Ok(age) => age >= Duration::from_secs(GC_GRACE_SECS),
                Err(_) => false,
            },
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// Recursively collect regular files under `root`.
async fn walk_files(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut rd = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            let path = entry.path();
            if entry.file_type().await?.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    Ok(())
}

async fn worker_loop(
    worker: &str,
    pool: PgPool,
    store: Arc<dyn Store>,
    job_timeout: Duration,
    egress: toottok_federation::EgressGuard,
    base_url: String,
) {
    loop {
        match Job::claim_next_due(&pool, worker).await {
            Ok(Some(job)) => {
                process_job_with_egress(&pool, &store, &egress, &base_url, job, job_timeout).await;
            }
            Ok(None) => tokio::time::sleep(POLL_EMPTY_DELAY).await,
            Err(e) => {
                warn!(worker, error = %e, "job claim failed; backing off");
                tokio::time::sleep(POLL_EMPTY_DELAY).await;
            }
        }
    }
}

/// Process one claimed job, bounded by `job_timeout`. On timeout the job is
/// treated as a failure: the clip (when present) is marked `failed` and the
/// job's `attempts` is bumped (dead once it passes `max_attempts`).
///
/// Used by the media test harness (probe/transcode/finalize only); federation
/// delivery jobs run through [`process_job_with_egress`] with the real egress
/// guard. `base_url` feeds the finalize-time federation Create.
pub async fn process_job(pool: &PgPool, store: &Arc<dyn Store>, job: Job, job_timeout: Duration) {
    process_job_with_egress(
        pool,
        store,
        &toottok_federation::EgressGuard::new(false),
        "https://toottok.test",
        job,
        job_timeout,
    )
    .await
}

/// Process one claimed job, bounded by `job_timeout`, with an egress guard for
/// any federation delivery work.
pub async fn process_job_with_egress(
    pool: &PgPool,
    store: &Arc<dyn Store>,
    egress: &toottok_federation::EgressGuard,
    base_url: &str,
    job: Job,
    job_timeout: Duration,
) {
    let clip_id = job.payload.get("clip_id").and_then(|v| v.as_i64());
    match tokio::time::timeout(
        job_timeout,
        handle_job(pool, store, egress, base_url, job.clone()),
    )
    .await
    {
        Ok(()) => {}
        Err(_) => {
            error!(id = job.id, "job exceeded job timeout; treating as failed");
            bump_job_after_failure(pool, &job, clip_id, "job timed out").await;
        }
    }
}

async fn handle_job(
    pool: &PgPool,
    store: &Arc<dyn Store>,
    egress: &toottok_federation::EgressGuard,
    base_url: &str,
    job: Job,
) {
    if job.kind.starts_with("deliver") {
        process_deliver(pool, egress, job).await;
        return;
    }
    match job.kind.as_str() {
        "probe" => process_probe(pool, store, job).await,
        "transcode" => process_transcode(pool, store, job).await,
        "finalize" => process_finalize(pool, base_url, job).await,
        other => {
            warn!(id = job.id, kind = %other, "unknown job kind; dead-lettering");
            let clip_id = job.payload.get("clip_id").and_then(|v| v.as_i64());
            if let Some(clip_id) = clip_id {
                if let Err(e) = Clip::mark_failed(pool, clip_id).await {
                    error!(id = job.id, clip_id, error = %e, "failed to fail clip for unknown job kind");
                }
            }
            if let Err(e) =
                Job::dead_letter(pool, job.id, &format!("unknown job kind: {other}")).await
            {
                error!(id = job.id, error = %e, "failed to dead-letter job");
            }
        }
    }
}

/// Timeout/overrun failure path. The UPDATE is guarded on the exact lock this
/// worker holds (`id`, `locked_by`, `state = 'running'`) so a job the reaper
/// already requeued (or a second worker reclaimed) is NOT double-bumped and its
/// clip is NOT failed by a worker that no longer owns the attempt. Only a
/// worker that actually bumps (1 row affected) flips the clip to `failed`.
pub async fn bump_job_after_failure(pool: &PgPool, job: &Job, clip_id: Option<i64>, reason: &str) {
    let bumped = match sqlx::query(
        r#"
        UPDATE jobs
        SET attempts = attempts + 1,
            state = CASE WHEN attempts + 1 >= max_attempts THEN 'dead' ELSE 'queued' END,
            last_error = CASE WHEN attempts + 1 >= max_attempts THEN $3 ELSE last_error END,
            locked_by = NULL,
            locked_at = NULL
        WHERE id = $1 AND locked_by = $2 AND state = 'running'
        "#,
    )
    .bind(job.id)
    .bind(&job.locked_by)
    .bind(reason)
    .execute(pool)
    .await
    {
        Ok(result) => result.rows_affected() == 1,
        Err(e) => {
            error!(id = job.id, error = %e, "failed to bump job after failure");
            return;
        }
    };

    if !bumped {
        warn!(
            id = job.id,
            "job already requeued/reclaimed; skipping failure bump"
        );
        return;
    }
    if let Some(clip_id) = clip_id {
        if let Err(e) = Clip::mark_failed(pool, clip_id).await {
            error!(id = job.id, clip_id, error = %e, "failed to fail clip after job failure");
        }
    }
}

async fn process_probe(pool: &PgPool, store: &Arc<dyn Store>, job: Job) {
    let clip_id = job.payload.get("clip_id").and_then(|v| v.as_i64());
    let path = job.payload.get("path").and_then(|v| v.as_str());
    let key = job
        .payload
        .get("key")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mime = job
        .payload
        .get("mime")
        .and_then(|v| v.as_str())
        .unwrap_or("video/mp4");

    let Some((clip_id, path)) = clip_id.zip(path) else {
        warn!(id = job.id, "malformed probe payload");
        if let Some(cid) = clip_id {
            let _ = Clip::mark_failed(pool, cid).await;
        }
        if let Err(e) = Job::dead_letter(
            pool,
            job.id,
            "malformed probe payload: clip_id/path missing",
        )
        .await
        {
            error!(id = job.id, error = %e, "failed to dead-letter malformed probe job");
        }
        return;
    };

    let cap_seconds =
        match numeric_setting(pool, "clip_max_seconds", CLIP_MAX_SECONDS_DEFAULT).await {
            Ok(v) => v,
            Err(e) => {
                reject_clip(
                    pool,
                    store,
                    job,
                    clip_id,
                    key.as_deref(),
                    &format!("settings read failed: {e}"),
                )
                .await;
                return;
            }
        };
    match probe::probe(Path::new(path)).await {
        Ok(info) => match probe::decide(&info, cap_seconds) {
            ProbeDecision::Accept => {
                match Clip::update_probe_info(
                    pool,
                    clip_id,
                    info.duration_s,
                    info.width,
                    info.height,
                )
                .await
                {
                    Ok(_) => {
                        let Some(original_key) = key.as_deref() else {
                            warn!(
                                id = job.id,
                                "probe payload missing storage key; cannot transcode"
                            );
                            let _ = Clip::mark_failed(pool, clip_id).await;
                            let _ = Job::mark_done(
                                pool,
                                job.id,
                                Some("probe payload missing storage key"),
                            )
                            .await;
                            return;
                        };
                        let payload = json!({
                            "clip_id": clip_id,
                            "path": path,
                            "key": original_key,
                            "mime": mime,
                        });
                        match Job::create(pool, "transcode", &payload, None).await {
                            Ok(_) => {
                                if let Err(e) = Job::mark_done(pool, job.id, None).await {
                                    error!(id = job.id, error = %e, "failed to mark probe job done");
                                }
                            }
                            Err(e) => {
                                reject_clip(
                                    pool,
                                    store,
                                    job,
                                    clip_id,
                                    Some(original_key),
                                    &format!("transcode enqueue failed: {e}"),
                                )
                                .await;
                            }
                        }
                    }
                    Err(e) => {
                        dead_letter_probe(pool, job, &format!("probe stamp failed: {e}")).await;
                    }
                }
            }
            ProbeDecision::Reject(reason) => {
                reject_clip(pool, store, job, clip_id, key.as_deref(), reason).await;
            }
        },
        Err(e) => {
            reject_clip(
                pool,
                store,
                job,
                clip_id,
                key.as_deref(),
                &format!("probe failed: {e}"),
            )
            .await;
        }
    }
}

/// REJECT path: over-cap duration or undecodable ⇒ clip `failed`, stored file
/// deleted, reason recorded in `jobs.last_error`, job marked done.
async fn reject_clip(
    pool: &PgPool,
    store: &Arc<dyn Store>,
    job: Job,
    clip_id: i64,
    key: Option<&str>,
    reason: &str,
) {
    if let Err(e) = Clip::mark_failed(pool, clip_id).await {
        error!(id = job.id, clip_id, error = %e, "failed to mark clip failed");
    }
    if let Some(key) = key {
        if let Err(e) = store.delete(key).await {
            warn!(id = job.id, clip_id, key, error = %e, "failed to delete rejected clip file");
        }
    }
    if let Err(e) = Job::mark_done(pool, job.id, Some(reason)).await {
        error!(id = job.id, error = %e, "failed to mark reject job done");
    }
}

/// Transcode: run the ladder into a scratch dir, move outputs under
/// `media_dir/renditions/{clip_id}/`, register every asset (ladder rungs +
/// poster + an `orig` row for the original file), then enqueue `finalize`.
/// Any failure ⇒ clip `failed`, partial outputs cleaned, error in
/// `jobs.last_error`.
async fn process_transcode(pool: &PgPool, store: &Arc<dyn Store>, job: Job) {
    let clip_id = job.payload.get("clip_id").and_then(|v| v.as_i64());
    let path = job
        .payload
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let key = job
        .payload
        .get("key")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let mime = job
        .payload
        .get("mime")
        .and_then(|v| v.as_str())
        .unwrap_or("video/mp4")
        .to_string();

    let Some(((clip_id, path), key)) = clip_id.zip(path).zip(key) else {
        warn!(id = job.id, "malformed transcode payload");
        if let Some(cid) = clip_id {
            let _ = Clip::mark_failed(pool, cid).await;
        }
        if let Err(e) = Job::dead_letter(
            pool,
            job.id,
            "malformed transcode payload: clip_id/path/key missing",
        )
        .await
        {
            error!(id = job.id, error = %e, "failed to dead-letter malformed transcode job");
        }
        return;
    };

    let workdir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            fail_transcode(
                pool,
                store,
                job,
                clip_id,
                &key,
                &format!("scratch dir failed: {e}"),
            )
            .await;
            return;
        }
    };

    let outputs = match transcode::transcode(Path::new(&path), workdir.path()).await {
        Ok(o) => o,
        Err(e) => {
            fail_transcode(
                pool,
                store,
                job,
                clip_id,
                &key,
                &format!("transcode failed: {e}"),
            )
            .await;
            return;
        }
    };

    let mut failure: Option<String> = None;

    for video in &outputs.videos {
        let spec = AssetSpec {
            kind: "video_mp4",
            rendition: video.rendition,
            filename: &format!("{}.mp4", video.rendition),
            mime: "video/mp4",
            codec: Some("h264"),
            source: &video.path,
        };
        if let Err(e) = persist_asset(pool, store, clip_id, &spec).await {
            failure = Some(e);
            break;
        }
    }

    if failure.is_none() {
        let spec = AssetSpec {
            kind: "poster",
            rendition: "orig",
            filename: "poster.jpg",
            mime: "image/jpeg",
            codec: None,
            source: &outputs.poster_path,
        };
        failure = persist_asset(pool, store, clip_id, &spec).await.err();
    }

    if failure.is_none() {
        let orig_size = store.open(&key).await.map(|f| f.size_bytes as i64).ok();
        match MediaAsset::upsert(
            pool,
            clip_id,
            "video_mp4",
            "orig",
            &key,
            &mime,
            orig_size,
            None,
            None,
        )
        .await
        {
            Ok(_) => {}
            Err(e) => failure = Some(e.to_string()),
        }
    }

    if let Some(reason) = failure {
        fail_transcode(pool, store, job, clip_id, &key, &reason).await;
        return;
    }

    let payload = json!({ "clip_id": clip_id });
    match Job::create(pool, "finalize", &payload, None).await {
        Ok(_) => {
            if let Err(e) = Job::mark_done(pool, job.id, None).await {
                error!(id = job.id, error = %e, "failed to mark transcode job done");
            }
        }
        Err(e) => {
            fail_transcode(
                pool,
                store,
                job,
                clip_id,
                &key,
                &format!("finalize enqueue failed: {e}"),
            )
            .await;
        }
    }
}

/// Where to land one produced file: media_assets columns + the on-disk source.
struct AssetSpec<'a> {
    kind: &'a str,
    rendition: &'a str,
    filename: &'a str,
    mime: &'a str,
    codec: Option<&'a str>,
    source: &'a std::path::Path,
}

/// Read a produced file, save it under `renditions/{clip_id}/{filename}`, and
/// register the `media_assets` row. `Err` carries a user-facing reason.
async fn persist_asset(
    pool: &PgPool,
    store: &Arc<dyn Store>,
    clip_id: i64,
    spec: &AssetSpec<'_>,
) -> Result<MediaAsset, String> {
    let data = tokio::fs::read(spec.source)
        .await
        .map_err(|e| e.to_string())?;
    let asset_key = format!("renditions/{clip_id}/{}", spec.filename);
    let stored = store
        .save_bytes(&asset_key, &data)
        .await
        .map_err(|e| e.to_string())?;
    MediaAsset::create(
        pool,
        clip_id,
        spec.kind,
        spec.rendition,
        &asset_key,
        spec.mime,
        Some(stored.size_bytes as i64),
        None,
        spec.codec,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Finalize: stamp every asset `ready_at`, flip the clip to `ready`, and —
/// for local clips — announce it: canonicalize the ap_id, log the outbound
/// Create(Note) activity, and enqueue a `deliver_create` fan-out job.
async fn process_finalize(pool: &PgPool, base_url: &str, job: Job) {
    let clip_id = job.payload.get("clip_id").and_then(|v| v.as_i64());
    let Some(clip_id) = clip_id else {
        warn!(id = job.id, "malformed finalize payload");
        if let Err(e) =
            Job::dead_letter(pool, job.id, "malformed finalize payload: clip_id missing").await
        {
            error!(id = job.id, error = %e, "failed to dead-letter malformed finalize job");
        }
        return;
    };

    if let Err(e) = MediaAsset::mark_ready_for_clip(pool, clip_id).await {
        let _ = Clip::mark_failed(pool, clip_id).await;
        if let Err(err) =
            Job::mark_done(pool, job.id, Some(&format!("asset stamp failed: {e}"))).await
        {
            error!(id = job.id, error = %err, "failed to mark finalize job done");
        }
        return;
    }

    match Clip::mark_ready(pool, clip_id).await {
        Ok(_) => {
            // Wave B: federate the finalized local clip. Best-effort at this
            // stage — an enqueue failure is recorded on the job for admin
            // visibility instead of failing an already-playable clip.
            if let Err(e) = enqueue_clip_create(pool, base_url, clip_id).await {
                error!(clip_id, error = %e, "failed to enqueue federation Create");
                if let Err(err) = Job::mark_done(
                    pool,
                    job.id,
                    Some(&format!("federation enqueue failed: {e}")),
                )
                .await
                {
                    error!(id = job.id, error = %err, "failed to mark finalize job done");
                }
                return;
            }
            if let Err(e) = Job::mark_done(pool, job.id, None).await {
                error!(id = job.id, error = %e, "failed to mark finalize job done");
            }
        }
        Err(e) => {
            let _ = Clip::mark_failed(pool, clip_id).await;
            if let Err(err) =
                Job::mark_done(pool, job.id, Some(&format!("finalize failed: {e}"))).await
            {
                error!(id = job.id, error = %err, "failed to mark finalize job done");
            }
        }
    }
}

/// Build the outbound `Create(Note)` for a finalized LOCAL clip: canonicalize
/// its `ap_id`, store the outbound activity, and enqueue the `deliver_create`
/// fan-out. Remote clips are never announced (we only cache them).
async fn enqueue_clip_create(pool: &PgPool, base_url: &str, clip_id: i64) -> Result<(), String> {
    use toottok_federation::activity::activity_id_from_json;
    use toottok_federation::note;

    let clip = Clip::fetch_by_id(pool, clip_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("clip {clip_id} vanished before finalize federation"))?;
    if clip.origin != "local" || clip.deleted_at.is_some() {
        return Ok(());
    }

    let author = toottok_db::actor::Actor::fetch_by_id(pool, clip.actor_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("clip {clip_id} has no author row"))?;
    if author.domain.is_some() {
        return Ok(());
    }

    // Canonical object id ({base}/clips/{id}); the upload-time placeholder is
    // not routable and would break fetch-side verification elsewhere.
    let canonical = note::clip_object_id(base_url, clip_id);
    if clip.ap_id != canonical {
        Clip::set_ap_id(pool, clip_id, &canonical)
            .await
            .map_err(|e| e.to_string())?;
    }
    let clip = Clip::fetch_by_id(pool, clip_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("clip {clip_id} vanished after ap_id update"))?;

    let media_filename = MediaAsset::largest_video_filename(pool, clip_id)
        .await
        .ok()
        .flatten()
        .or_else(|| Some("720.mp4".to_string()));

    let activity = note::clip_create_activity(base_url, &clip, &author, media_filename.as_deref());
    let activity_id = activity_id_from_json(&activity);
    let _ = toottok_db::activity::Activity::create_outbound(
        pool,
        &activity_id,
        &author.ap_id,
        Some(&canonical),
        &activity,
    )
    .await;

    Job::create(
        pool,
        "deliver_create",
        &serde_json::json!({ "clip_id": clip_id, "activity": activity }),
        None,
    )
    .await
    .map_err(|e| e.to_string())?;
    info!(clip_id, activity_id, "Create(Note) enqueued for delivery");
    Ok(())
}

/// Transcode/finalize failure path: clip `failed`, partial outputs + asset
/// rows cleaned, reason in `jobs.last_error`, job marked done.
async fn fail_transcode(
    pool: &PgPool,
    store: &Arc<dyn Store>,
    job: Job,
    clip_id: i64,
    original_key: &str,
    reason: &str,
) {
    let _ = MediaAsset::delete_for_clip(pool, clip_id).await;
    for partial in ["720.mp4", "480.mp4", "poster.jpg"] {
        let _ = store
            .delete(&format!("renditions/{clip_id}/{partial}"))
            .await;
    }
    let _ = store.delete(original_key).await;
    if let Err(e) = Clip::mark_failed(pool, clip_id).await {
        error!(id = job.id, clip_id, error = %e, "failed to mark clip failed");
    }
    if let Err(e) = Job::mark_done(pool, job.id, Some(reason)).await {
        error!(id = job.id, error = %e, "failed to mark failed transcode job done");
    }
}

async fn dead_letter_probe(pool: &PgPool, job: Job, reason: &str) {
    if let Err(e) = Job::dead_letter(pool, job.id, reason).await {
        error!(id = job.id, error = %e, "failed to dead-letter probe job");
    }
}

/// Process a federation delivery job (`deliver` / `deliver_follow` /
/// `deliver_create`): sign the activity with the signer's key and POST it to
/// the target inbox through the egress guard. Permanent client rejections end
/// the job (admin-visible in `last_error`); transient failures are retried
/// with exponential backoff (30s → 2m → 15m) and dead-lettered at
/// `max_attempts`.
async fn process_deliver(pool: &PgPool, egress: &toottok_federation::EgressGuard, job: Job) {
    if job.kind == "deliver_create" {
        process_deliver_create(pool, job).await;
        return;
    }
    use toottok_federation::deliver::{deliver_job, DeliverOutcome};
    match deliver_job(pool, egress, &job).await {
        Ok(DeliverOutcome::Delivered) => {
            if let Err(e) = Job::mark_done(pool, job.id, None).await {
                error!(id = job.id, error = %e, "failed to mark deliver job done");
            }
        }
        Ok(DeliverOutcome::Rejected(reason)) => {
            if let Err(e) = Job::mark_done(pool, job.id, Some(&reason)).await {
                error!(id = job.id, error = %e, "failed to mark rejected deliver job done");
            }
        }
        Ok(DeliverOutcome::Failed(reason)) => schedule_deliver_retry(pool, job, &reason).await,
        Err(e) => schedule_deliver_retry(pool, job, &e.detail()).await,
    }
}

/// Fan a finalized clip's `Create(Note)` out to every remote follower's
/// shared (or personal) inbox. The activity is signed AS THE AUTHOR (never
/// the instance actor); each per-inbox delivery reuses the plain `deliver`
/// machinery — signing, egress guard, instances bookkeeping, backoff and
/// dead-lettering included.
async fn process_deliver_create(pool: &PgPool, job: Job) {
    use toottok_federation::deliver::{enqueue_delivery, shared_inbox_or_inbox};

    let clip_id = job.payload.get("clip_id").and_then(|v| v.as_i64());
    let activity = job.payload.get("activity").cloned();
    let Some((clip_id, activity)) = clip_id.zip(activity) else {
        warn!(id = job.id, "malformed deliver_create payload");
        if let Err(e) = Job::dead_letter(pool, job.id, "malformed deliver_create payload").await {
            error!(id = job.id, error = %e, "failed to dead-letter malformed deliver_create");
        }
        return;
    };

    let Some(clip) = Clip::fetch_by_id(pool, clip_id).await.ok().flatten() else {
        if let Err(e) = Job::dead_letter(pool, job.id, "deliver_create: clip not found").await {
            error!(id = job.id, error = %e, "failed to dead-letter deliver_create");
        }
        return;
    };
    let author = match toottok_db::actor::Actor::fetch_by_id(pool, clip.actor_id).await {
        Ok(Some(a)) => a,
        _ => {
            if let Err(e) =
                Job::dead_letter(pool, job.id, "deliver_create: author actor not found").await
            {
                error!(id = job.id, error = %e, "failed to dead-letter deliver_create");
            }
            return;
        }
    };

    // Follower shared inboxes of the AUTHOR (remote followers only; local
    // followers see the clip directly in their feeds).
    let followers = match Follow::remote_follower_actors(pool, author.id).await {
        Ok(f) => f,
        Err(e) => {
            error!(clip_id, error = %e, "deliver_create: follower query failed");
            schedule_deliver_retry(pool, job, &format!("follower query failed: {e}")).await;
            return;
        }
    };

    let mut failures = 0usize;
    for follower in &followers {
        let inbox = shared_inbox_or_inbox(follower);
        if let Err(e) = enqueue_delivery(pool, author.id, &inbox, &activity).await {
            failures += 1;
            error!(
                clip_id,
                inbox = %inbox,
                error = %e,
                "deliver_create: failed to enqueue per-inbox delivery"
            );
        }
    }

    let note = (failures > 0)
        .then(|| format!("{failures} of {} fan-out enqueues failed", followers.len()));
    if let Err(e) = Job::mark_done(pool, job.id, note.as_deref()).await {
        error!(id = job.id, error = %e, "failed to mark deliver_create done");
    }
}

/// Exponential backoff for a failed delivery: attempts 0,1,2 → 30s, 2m, 15m;
/// further attempts dead-letter. `attempts` counts completed tries, so after a
/// failure at `attempts` the retry interval is `backoff(attempts)`.
async fn schedule_deliver_retry(pool: &PgPool, job: Job, reason: &str) {
    let backoff_secs = match job.attempts {
        0 => 30,
        1 => 120,
        _ => 900,
    };
    if job.attempts + 1 >= job.max_attempts {
        warn!(id = job.id, kind = %job.kind, "delivery dead-lettered: {reason}");
        if let Err(e) = Job::dead_letter(pool, job.id, reason).await {
            error!(id = job.id, error = %e, "failed to dead-letter deliver job");
        }
        return;
    }
    if let Err(e) = sqlx::query(
        r#"
        UPDATE jobs
        SET state = 'queued',
            attempts = attempts + 1,
            last_error = $2,
            run_after = now() + ($1::int * interval '1 second'),
            locked_by = NULL,
            locked_at = NULL
        WHERE id = $3
        "#,
    )
    .bind(backoff_secs)
    .bind(reason)
    .bind(job.id)
    .execute(pool)
    .await
    {
        error!(id = job.id, error = %e, "failed to schedule deliver retry");
    }
}
