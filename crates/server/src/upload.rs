//! POST /api/v1/clips/upload — multipart upload pipeline (Phase 4: requires a
//! logged-in session; the clip is attributed to the authed actor):
//! magic-byte sniff → size cap (settings) → sha256 dedup → storage quota →
//! store original → insert pending local clip → enqueue `probe` job.

use axum::extract::{Multipart, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sqlx::PgPool;
use toottok_db::clip::Clip;
use toottok_db::error::DbError;
use toottok_db::job::Job;
use toottok_db::media_asset::MediaAsset;
use toottok_media::probe;
use toottok_media::store::sha256_hex;
use tracing::warn;
use uuid::Uuid;

use crate::problem::problem;
use crate::session::AuthUser;
use crate::settings::numeric_setting;
use crate::AppState;

/// Default upload cap in MB (admin-adjustable via settings `upload_size_cap_mb`).
pub const UPLOAD_SIZE_CAP_MB_DEFAULT: f64 = 500.0;

/// Default per-user storage quota in MB; `0` means unlimited
/// (settings `per_user_storage_quota_mb`).
pub const PER_USER_STORAGE_QUOTA_MB_DEFAULT: f64 = 0.0;

pub async fn upload(
    State(state): State<AppState>,
    auth: AuthUser,
    mut multipart: Multipart,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };

    let actor_id = auth.actor.id;

    let size_cap_bytes = match upload_size_cap_bytes(pool).await {
        Ok(v) => v,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    let (data, caption_raw) = match read_upload_fields(&mut multipart, size_cap_bytes).await {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let Some(data) = data else {
        return problem(
            StatusCode::BAD_REQUEST,
            "missing file",
            "multipart field 'file' is required",
        );
    };
    if data.is_empty() {
        return problem(
            StatusCode::BAD_REQUEST,
            "empty file",
            "uploaded file is empty",
        );
    }

    let container = match probe::sniff_container(&data) {
        Ok(c) => c,
        Err(_) => {
            return problem(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported container",
                "only mp4, webm, and mov uploads are accepted",
            )
        }
    };

    let hash = sha256_hex(&data);
    match Clip::fetch_by_sha256(pool, &hash).await {
        Ok(Some(_)) => {
            return problem(
                StatusCode::CONFLICT,
                "duplicate upload",
                "an identical clip has already been uploaded",
            )
        }
        Ok(None) => {}
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    }

    if let Err(reason) = check_storage_quota(pool, actor_id, data.len() as i64).await {
        return reason;
    }

    let key = format!("original/{}.{}", Uuid::new_v4(), container.ext);
    let stored = match state.store.save_bytes(&key, &data).await {
        Ok(f) => f,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage error",
                format!("{e}"),
            )
        }
    };

    // Captions are sanitized by stripping ALL tags (v1 stance, mirrors the
    // inbound federation path); an empty result stores NULL.
    let caption_html = caption_raw
        .as_deref()
        .map(toottok_federation::note::strip_html_tags)
        .filter(|s| !s.is_empty());

    let ap_id = format!("https://toottok.local/clips/{}", Uuid::new_v4());
    let clip = match Clip::create_pending_upload(
        pool,
        actor_id,
        &ap_id,
        &hash,
        data.len() as i64,
        caption_html.as_deref(),
    )
    .await
    {
        Ok(c) => c,
        Err(e) if e.is_unique_violation() => {
            let _ = state.store.delete(&key).await;
            return problem(
                StatusCode::CONFLICT,
                "duplicate upload",
                "an identical clip has already been uploaded",
            );
        }
        Err(e) => {
            let _ = state.store.delete(&key).await;
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    };

    if let Some(cap) = caption_html.as_deref() {
        if let Err(e) = toottok_db::hashtag::link_hashtags_to_clip(pool, clip.id, cap).await {
            let _ = state.store.delete(&key).await;
            let _ = Clip::mark_failed(pool, clip.id).await;
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    }

    let payload = json!({
        "clip_id": clip.id,
        "path": stored.path.to_string_lossy(),
        "key": key,
        "mime": container.mime,
    });
    if let Err(e) = Job::create(pool, "probe", &payload, None).await {
        let _ = state.store.delete(&key).await;
        let _ = Clip::mark_failed(pool, clip.id).await;
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }

    // Register the original as a media_asset row the moment the file lands so
    // the GC (which only deletes files absent from media_assets.path) cannot
    // eat an in-flight upload whose transcode hasn't run yet. `ready_at` stays
    // NULL until finalize stamps it. Failure here is non-fatal: the pipeline
    // self-heals at transcode via `upsert`, and the GC grace window protects
    // the fresh file regardless.
    if let Err(e) = MediaAsset::create(
        pool,
        clip.id,
        "video_mp4",
        "orig",
        &key,
        container.mime,
        Some(stored.size_bytes as i64),
        None,
        None,
    )
    .await
    {
        warn!(clip_id = clip.id, error = %e, "failed to register early orig asset; GC grace covers it");
    }

    (
        StatusCode::CREATED,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        serde_json::to_vec(&json!({ "clip_id": clip.id, "status": "pending" }))
            .expect("response serialization cannot fail"),
    )
        .into_response()
}

/// Stream the upload's multipart fields: the `file` part (size-capped
/// mid-stream) plus the optional `caption_html` text field. `pub(crate)` so
/// the avatar handler reuses the file reader.
pub(crate) async fn read_file_field(
    multipart: &mut Multipart,
    size_cap_bytes: usize,
) -> Result<Option<Vec<u8>>, Response> {
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| problem(StatusCode::BAD_REQUEST, "invalid multipart", format!("{e}")))?
    {
        let is_file = field.name().is_some_and(|n| n.eq_ignore_ascii_case("file"));
        if !is_file {
            continue;
        }
        return read_file_chunks(&mut field, size_cap_bytes).await;
    }
    Ok(None)
}

/// Upload-specific field reader: collects the `file` bytes AND the optional
/// `caption_html` text (rejected above [`CAPTION_MAX_CHARS`] so a hostile
/// text field cannot be buffered unboundedly).
async fn read_upload_fields(
    multipart: &mut Multipart,
    size_cap_bytes: usize,
) -> Result<(Option<Vec<u8>>, Option<String>), Response> {
    let mut file: Option<Vec<u8>> = None;
    let mut caption: Option<String> = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|e| problem(StatusCode::BAD_REQUEST, "invalid multipart", format!("{e}")))?
    {
        match field.name() {
            Some(n) if n.eq_ignore_ascii_case("file") => {
                file = read_file_chunks(&mut field, size_cap_bytes).await?;
            }
            Some(n) if n.eq_ignore_ascii_case("caption_html") => {
                let mut buf = Vec::new();
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            if buf.len() + chunk.len() > CAPTION_MAX_BYTES {
                                return Err(problem(
                                    StatusCode::PAYLOAD_TOO_LARGE,
                                    "caption too large",
                                    format!("caption exceeds {CAPTION_MAX_BYTES} bytes"),
                                ));
                            }
                            buf.extend_from_slice(&chunk);
                        }
                        Ok(None) => break,
                        Err(e) => {
                            return Err(problem(
                                StatusCode::BAD_REQUEST,
                                "invalid multipart",
                                format!("{e}"),
                            ))
                        }
                    }
                }
                caption = String::from_utf8(buf).ok().filter(|s| !s.is_empty());
            }
            _ => {}
        }
    }
    Ok((file, caption))
}

/// Hard buffer bound for the caption field.
const CAPTION_MAX_BYTES: usize = 16 * 1024;

/// Read one field's chunks with the size cap enforced mid-stream so a hostile
/// upload is rejected before it is fully buffered.
async fn read_file_chunks(
    field: &mut axum::extract::multipart::Field<'_>,
    size_cap_bytes: usize,
) -> Result<Option<Vec<u8>>, Response> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                if buf.len().saturating_add(chunk.len()) > size_cap_bytes {
                    return Err(problem(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "upload too large",
                        format!("file exceeds the {size_cap_bytes}-byte upload cap"),
                    ));
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(e) => {
                return Err(problem(
                    StatusCode::BAD_REQUEST,
                    "invalid multipart",
                    format!("{e}"),
                ))
            }
        }
    }
    Ok(Some(buf))
}

async fn upload_size_cap_bytes(pool: &PgPool) -> Result<usize, DbError> {
    let mb = numeric_setting(pool, "upload_size_cap_mb", UPLOAD_SIZE_CAP_MB_DEFAULT).await?;
    Ok((mb * 1024.0 * 1024.0) as usize)
}

/// Enforce `per_user_storage_quota_mb` (0 = unlimited). The current upload's
/// bytes count against the quota; over-quota answers 413 problem+json with
/// `storage_quota_exceeded`.
async fn check_storage_quota(
    pool: &PgPool,
    actor_id: i64,
    upload_bytes: i64,
) -> Result<(), Response> {
    let quota_mb = numeric_setting(
        pool,
        "per_user_storage_quota_mb",
        PER_USER_STORAGE_QUOTA_MB_DEFAULT,
    )
    .await
    .map_err(|e| {
        problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        )
    })?;
    if quota_mb <= 0.0 {
        return Ok(());
    }
    let quota_bytes = (quota_mb * 1024.0 * 1024.0) as i64;

    let used: i64 = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COALESCE(SUM(ma.size_bytes), 0)::bigint
        FROM media_assets ma
        JOIN clips c ON c.id = ma.clip_id
        WHERE c.actor_id = $1 AND c.deleted_at IS NULL AND c.status <> 'failed'
        "#,
    )
    .bind(actor_id)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        )
    })?;

    if used.saturating_add(upload_bytes) > quota_bytes {
        return Err(problem(
            StatusCode::PAYLOAD_TOO_LARGE,
            "storage_quota_exceeded",
            format!(
                "upload of {upload_bytes} bytes would exceed the {quota_bytes}-byte per-user storage quota"
            ),
        ));
    }
    Ok(())
}
