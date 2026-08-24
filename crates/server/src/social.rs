//! Feed, like, announce, comment, notification, search, report, and profile
//! APIs: `/api/v1/feed/{following,discover}`, `/api/v1/clips/{id}/{like,
//! announce,comments}`, `/api/v1/comments/{id}`, `/api/v1/notifications`,
//! `/api/v1/search`, `/api/v1/reports`, `/api/v1/admin/reports`, and
//! `/api/v1/profiles/{username}`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use toottok_db::clip::Clip;
use toottok_db::comment::Comment;
use toottok_db::error::DbError;
use toottok_db::feed::{self, FeedClipRow};
use toottok_db::follow::Follow;
use toottok_db::media_asset::MediaAsset;
use toottok_federation::note::strip_html_tags;
use uuid::Uuid;

use crate::problem::problem;
use crate::session::{AuthUser, OptionalAuthUser};
use crate::AppState;

const FEED_DEFAULT_LIMIT: i64 = 20;
const FEED_MAX_LIMIT: i64 = 50;

const COMMENTS_DEFAULT_LIMIT: i64 = 50;
const COMMENTS_MAX_LIMIT: i64 = 100;
const COMMENT_MAX_CHARS: usize = 1000;

const NOTIFICATIONS_DEFAULT_LIMIT: i64 = 30;
const NOTIFICATIONS_MAX_LIMIT: i64 = 50;

const REPORT_BODY_MAX_CHARS: usize = 2000;

/// Build one feed-card JSON object from a [`FeedClipRow`], resolving media
/// URLs: local clips point at their best ready rendition (falling back to the
/// original upload), remote clips hot-link `remote_media_url`. `poster_url`
/// is present when a poster asset exists. Sound attribution is fetched
/// separately (nullable, cheap indexed lookup) so the row struct stays a
/// plain SELECT mapping.
async fn feed_item(pool: &sqlx::PgPool, row: &FeedClipRow) -> Result<Value, DbError> {
    let mut asset_url = None;
    let mut poster_url = None;

    // Remote-origin cached clips hot-link their source URL; anything else
    // resolves against our own media store. (Local actors may carry the
    // instance domain rather than NULL, so domain alone can't decide this.)
    if let Some(remote) = row.remote_media_url.clone() {
        asset_url = Some(remote);
    } else {
        if let Some(filename) = MediaAsset::largest_video_filename(pool, row.id).await? {
            asset_url = Some(format!("/assets/{}/{}", row.id, filename));
        } else if let Some(orig) = sqlx::query_as::<_, MediaAsset>(
            "SELECT * FROM media_assets \
             WHERE clip_id = $1 AND kind = 'video_mp4' AND rendition = 'orig'",
        )
        .bind(row.id)
        .fetch_optional(pool)
        .await?
        {
            // No ready ladder rung: fall back to the original upload's
            // stored filename.
            let filename = orig.path.rsplit('/').next().unwrap_or(&orig.path);
            asset_url = Some(format!("/assets/{}/{}", row.id, filename));
        }

        if let Some(poster_path) = sqlx::query_scalar::<_, String>(
            "SELECT path FROM media_assets WHERE clip_id = $1 AND kind = 'poster'",
        )
        .bind(row.id)
        .fetch_optional(pool)
        .await?
        {
            let filename = poster_path.rsplit('/').next().unwrap_or(&poster_path);
            poster_url = Some(format!("/assets/{}/{}", row.id, filename));
        }
    }

    let sound = sqlx::query_as::<_, (i64, String)>(
        "SELECT s.id, s.title FROM clips c JOIN sounds s ON s.id = c.sound_id \
         WHERE c.id = $1",
    )
    .bind(row.id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    Ok(json!({
        "id": row.id,
        "ap_id": row.ap_id,
        "caption_html": row.caption_html,
        "duration_s": row.duration_s,
        "width": row.width,
        "height": row.height,
        "like_count": row.like_count,
        "comment_count": row.comment_count,
        "share_count": row.share_count,
        "created_at": row.clip_created_at,
        "sound": sound.map(|(id, title)| json!({ "id": id, "title": title })),
        "author": {
            "actor_id": row.actor_id,
            "username": row.username,
            "display_name": row.display_name,
            "avatar_path": row.avatar_path,
            "domain": row.domain,
        },
        "asset_url": asset_url,
        "poster_url": poster_url,
    }))
}

#[derive(Debug, Deserialize)]
pub struct FeedParams {
    pub cursor: Option<String>,
    pub page: Option<usize>,
}

/// Parse a `{timestamp_secs}-{id}` keyset cursor.
fn parse_cursor(cursor: &str) -> Option<(DateTime<Utc>, i64)> {
    let (secs, id) = cursor.split_once('-')?;
    let secs = secs.parse::<i64>().ok()?;
    let id = id.parse::<i64>().ok()?;
    Utc.timestamp_opt(secs, 0).single().map(|ts| (ts, id))
}

/// Page size from the `page` param: default 20, hard-capped at 50.
fn feed_limit(params: &FeedParams) -> i64 {
    params
        .page
        .map(|p| p as i64)
        .unwrap_or(FEED_DEFAULT_LIMIT)
        .clamp(1, FEED_MAX_LIMIT)
}

/// Split a cursor param into the `(before_created_at, before_id)` keyset pair,
/// or a ready 400 problem response.
#[allow(clippy::result_large_err)]
fn cursor_keyset(params: &FeedParams) -> Result<(Option<DateTime<Utc>>, Option<i64>), Response> {
    match params.cursor.as_deref() {
        None => Ok((None, None)),
        Some(c) => match parse_cursor(c) {
            Some((ts, id)) => Ok((Some(ts), Some(id))),
            None => Err(problem(
                StatusCode::BAD_REQUEST,
                "invalid cursor",
                "cursor must be '{unix_secs}-{clip_id}'",
            )),
        },
    }
}

/// Enrich rows into feed-card JSON values, or the 500 problem response.
#[allow(clippy::result_large_err)]
async fn feed_rows_to_values(
    pool: &sqlx::PgPool,
    rows: &[FeedClipRow],
) -> Result<Vec<Value>, Response> {
    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        match feed_item(pool, row).await {
            Ok(item) => items.push(item),
            Err(e) => {
                return Err(problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database error",
                    format!("{e}"),
                ))
            }
        }
    }
    Ok(items)
}

/// Enrich rows into cards and compute the next-page keyset cursor (present
/// only while the page came back full, so clients stop at the feed's end).
async fn render_feed(pool: &sqlx::PgPool, rows: Vec<FeedClipRow>, limit: i64) -> Response {
    let items = match feed_rows_to_values(pool, &rows).await {
        Ok(items) => items,
        Err(resp) => return resp,
    };

    let next_cursor = if rows.len() == limit as usize {
        rows.last()
            .map(|last| format!("{}-{}", last.clip_created_at.timestamp(), last.id))
    } else {
        None
    };

    Json(json!({ "items": items, "next_cursor": next_cursor })).into_response()
}

/// GET /api/v1/feed/following — newest clips from accepted follows.
pub async fn following_feed(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<FeedParams>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let (before_created_at, before_id) = match cursor_keyset(&params) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let limit = feed_limit(&params);

    match feed::following_feed(pool, auth.actor.id, before_created_at, before_id, limit).await {
        Ok(rows) => render_feed(pool, rows, limit).await,
        Err(e) => problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        ),
    }
}

/// GET /api/v1/feed/discover — newest public clips across all actors.
pub async fn discover_feed(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<FeedParams>,
) -> Response {
    let _ = auth;
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let (before_created_at, before_id) = match cursor_keyset(&params) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let limit = feed_limit(&params);

    match feed::discover_feed(pool, before_created_at, before_id, limit).await {
        Ok(rows) => render_feed(pool, rows, limit).await,
        Err(e) => problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        ),
    }
}

/// GET /api/v1/feed/local — newest public clips from LOCAL actors only
/// (fediverse "local timeline").
pub async fn local_feed(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<FeedParams>,
) -> Response {
    let _ = auth;
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let (before_created_at, before_id) = match cursor_keyset(&params) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let limit = feed_limit(&params);

    match feed::local_feed(pool, before_created_at, before_id, limit).await {
        Ok(rows) => render_feed(pool, rows, limit).await,
        Err(e) => problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        ),
    }
}

/// GET /api/v1/tags/{tag}/clips — public clips carrying the hashtag, same
/// feed-card shape and keyset pagination as the discover feed.
#[allow(clippy::result_large_err)]
pub async fn tag_clips(
    State(state): State<AppState>,
    Path(tag): Path<String>,
    Query(params): Query<FeedParams>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let (before_created_at, before_id) = match cursor_keyset(&params) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let limit = feed_limit(&params);

    match feed::clips_by_tag(pool, &tag, before_created_at, before_id, limit).await {
        Ok(rows) => render_feed(pool, rows, limit).await,
        Err(e) => problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        ),
    }
}

/// GET /api/v1/feed/trending — engagement-weighted, recency-decayed clips
/// for the Discover page grid.
pub async fn trending_feed(State(state): State<AppState>, auth: AuthUser) -> Response {
    let _ = auth;
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    match feed::trending(pool, 60).await {
        Ok(rows) => {
            let n = rows.len() as i64;
            render_feed(pool, rows, n).await
        }
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}")),
    }
}

/// GET /api/v1/tags/trending — hottest hashtags over the last 14 days.
pub async fn trending_tags(State(state): State<AppState>) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    match feed::trending_tags(pool, 12).await {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|(tag, uses)| json!({ "tag": tag, "uses": uses }))
                .collect();
            Json(json!({ "items": items })).into_response()
        }
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}")),
    }
}

/// GET /api/v1/sounds/{id} — sound card.
pub async fn sound_detail(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    match toottok_db::sound::fetch(pool, id).await {
        Ok(Some(s)) => Json(json!({
            "id": s.id,
            "title": s.title,
            "author": s.author_username,
            "clip_count": s.clip_count,
        }))
        .into_response(),
        Ok(None) => problem(StatusCode::NOT_FOUND, "sound not found", format!("no sound {id}")),
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}")),
    }
}

/// GET /api/v1/sounds/{id}/clips — feed-card page of clips using the sound.
#[allow(clippy::result_large_err)]
pub async fn sound_clips(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<FeedParams>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let (before_created_at, before_id) = match cursor_keyset(&params) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let limit = feed_limit(&params);
    match toottok_db::sound::clips_for_sound(pool, id, before_created_at, before_id, limit).await {
        Ok(rows) => render_feed(pool, rows, limit).await,
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}")),
    }
}

/// Fetch a live clip or produce the right problem response.
async fn fetch_live_clip(pool: &sqlx::PgPool, id: i64) -> Result<Clip, Response> {
    match Clip::fetch_by_id(pool, id).await {
        Ok(Some(c)) if c.deleted_at.is_none() => Ok(c),
        Ok(_) => Err(problem(
            StatusCode::NOT_FOUND,
            "clip not found",
            format!("no live clip with id {id}"),
        )),
        Err(e) => Err(problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        )),
    }
}

/// POST /api/v1/clips/{id}/like — idempotent like with counter + notification.
pub async fn like_clip(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let clip = match fetch_live_clip(pool, id).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    let inserted = match sqlx::query(
        "INSERT INTO likes (clip_id, actor_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(auth.actor.id)
    .execute(pool)
    .await
    {
        Ok(res) => res.rows_affected(),
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    if inserted == 0 {
        // Already liked: stay idempotent, report current state.
        return (
            StatusCode::OK,
            Json(json!({ "liked": true, "like_count": clip.like_count })),
        )
            .into_response();
    }

    let like_count = match sqlx::query_scalar::<_, i64>(
        "UPDATE clips SET like_count = like_count + 1 \
         WHERE id = $1 AND deleted_at IS NULL RETURNING like_count",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(n)) => n,
        Ok(None) => {
            return problem(
                StatusCode::NOT_FOUND,
                "clip not found",
                format!("no live clip with id {id}"),
            )
        }
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    if clip.actor_id != auth.actor.id {
        if let Err(e) = sqlx::query(
            "INSERT INTO notifications (actor_id, kind, source_actor_id, clip_id) \
             VALUES ($1, 'like', $2, $3)",
        )
        .bind(clip.actor_id)
        .bind(auth.actor.id)
        .bind(id)
        .execute(pool)
        .await
        {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    }

    (
        StatusCode::OK,
        Json(json!({ "liked": true, "like_count": like_count })),
    )
        .into_response()
}

/// DELETE /api/v1/clips/{id}/like — retract a like, counter floored at zero.
pub async fn unlike_clip(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let clip = match fetch_live_clip(pool, id).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    let removed = match sqlx::query("DELETE FROM likes WHERE clip_id = $1 AND actor_id = $2")
        .bind(id)
        .bind(auth.actor.id)
        .execute(pool)
        .await
    {
        Ok(res) => res.rows_affected(),
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    let like_count = if removed > 0 {
        match sqlx::query_scalar::<_, i64>(
            "UPDATE clips SET like_count = GREATEST(like_count - 1, 0) \
             WHERE id = $1 AND deleted_at IS NULL RETURNING like_count",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        {
            Ok(Some(n)) => n,
            Ok(None) => clip.like_count,
            Err(e) => {
                return problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database error",
                    format!("{e}"),
                )
            }
        }
    } else {
        clip.like_count
    };

    (
        StatusCode::OK,
        Json(json!({ "liked": false, "like_count": like_count })),
    )
        .into_response()
}

/// POST /api/v1/clips/{id}/announce — idempotent boost with counter + notification.
pub async fn announce_clip(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let clip = match fetch_live_clip(pool, id).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    let inserted = match sqlx::query(
        "INSERT INTO announces (clip_id, actor_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(auth.actor.id)
    .execute(pool)
    .await
    {
        Ok(res) => res.rows_affected(),
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    if inserted == 0 {
        // Already announced: stay idempotent, report current state.
        return (
            StatusCode::OK,
            Json(json!({ "announced": true, "share_count": clip.share_count })),
        )
            .into_response();
    }

    let share_count = match sqlx::query_scalar::<_, i64>(
        "UPDATE clips SET share_count = share_count + 1 \
         WHERE id = $1 AND deleted_at IS NULL RETURNING share_count",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    {
        Ok(Some(n)) => n,
        Ok(None) => {
            return problem(
                StatusCode::NOT_FOUND,
                "clip not found",
                format!("no live clip with id {id}"),
            )
        }
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    if clip.actor_id != auth.actor.id {
        if let Err(e) = sqlx::query(
            "INSERT INTO notifications (actor_id, kind, source_actor_id, clip_id) \
             VALUES ($1, 'boost', $2, $3)",
        )
        .bind(clip.actor_id)
        .bind(auth.actor.id)
        .bind(id)
        .execute(pool)
        .await
        {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    }

    (
        StatusCode::OK,
        Json(json!({ "announced": true, "share_count": share_count })),
    )
        .into_response()
}

/// DELETE /api/v1/clips/{id}/announce — retract a boost, counter floored at zero.
pub async fn unannounce_clip(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let clip = match fetch_live_clip(pool, id).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };

    let removed = match sqlx::query("DELETE FROM announces WHERE clip_id = $1 AND actor_id = $2")
        .bind(id)
        .bind(auth.actor.id)
        .execute(pool)
        .await
    {
        Ok(res) => res.rows_affected(),
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    let share_count = if removed > 0 {
        match sqlx::query_scalar::<_, i64>(
            "UPDATE clips SET share_count = GREATEST(share_count - 1, 0) \
             WHERE id = $1 AND deleted_at IS NULL RETURNING share_count",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        {
            Ok(Some(n)) => n,
            Ok(None) => clip.share_count,
            Err(e) => {
                return problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database error",
                    format!("{e}"),
                )
            }
        }
    } else {
        clip.share_count
    };

    (
        StatusCode::OK,
        Json(json!({ "announced": false, "share_count": share_count })),
    )
        .into_response()
}

// ---------------------------------------------------------------- bookmarks

/// PUT /api/v1/clips/{id}/bookmark — save a clip for the viewer.
pub async fn bookmark_clip(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if fetch_live_clip(pool, id).await.is_err() {
        return problem(StatusCode::NOT_FOUND, "clip not found", format!("no live clip {id}"));
    }
    match toottok_db::bookmark::add(pool, id, auth.actor.id).await {
        Ok(_) => (StatusCode::OK, Json(json!({ "bookmarked": true }))).into_response(),
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}")),
    }
}

/// DELETE /api/v1/clips/{id}/bookmark — drop the viewer's saved clip.
pub async fn unbookmark_clip(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    match toottok_db::bookmark::remove(pool, id, auth.actor.id).await {
        Ok(_) => (StatusCode::OK, Json(json!({ "bookmarked": false }))).into_response(),
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}")),
    }
}

/// GET /api/v1/bookmarks — the viewer's saved clips, feed-card shaped.
#[allow(clippy::result_large_err)]
pub async fn list_bookmarks(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<FeedParams>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let (before_created_at, before_id) = match cursor_keyset(&params) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let limit = feed_limit(&params);

    match toottok_db::bookmark::list(pool, auth.actor.id, before_created_at, before_id, limit).await
    {
        Ok(rows) => render_feed(pool, rows, limit).await,
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}")),
    }
}

// ------------------------------------------------------------------ comments

/// One comment page row with its author handle.
#[derive(Debug, sqlx::FromRow)]
struct CommentRow {
    id: i64,
    body_html: String,
    created_at: DateTime<Utc>,
    username: String,
    domain: Option<String>,
}

/// GET /api/v1/clips/{id}/comments — public oldest-first keyset page.
pub async fn list_comments(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<FeedParams>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let (after_created_at, after_id) = match cursor_keyset(&params) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let limit = params
        .page
        .map(|p| p as i64)
        .unwrap_or(COMMENTS_DEFAULT_LIMIT)
        .clamp(1, COMMENTS_MAX_LIMIT);

    let mut sql = String::from(
        "SELECT c.id, c.body_html, c.created_at, a.username, a.domain \
         FROM comments c JOIN actors a ON a.id = c.actor_id \
         WHERE c.clip_id = $1 AND c.deleted_at IS NULL",
    );
    if after_created_at.is_some() {
        sql.push_str(" AND (c.created_at, c.id) > ($2, $3)");
    }
    // The LIMIT placeholder index depends on whether the cursor bound $2/$3;
    // referencing a higher parameter than the highest bound one makes
    // Postgres unable to infer its type.
    let comments_limit_param = if after_created_at.is_some() {
        "$4"
    } else {
        "$2"
    };
    sql.push_str(&format!(
        " ORDER BY c.created_at ASC, c.id ASC LIMIT {comments_limit_param}"
    ));

    let mut q = sqlx::query_as::<_, CommentRow>(&sql).bind(id);
    if let (Some(ts), Some(cid)) = (after_created_at, after_id) {
        q = q.bind(ts).bind(cid);
    }
    let rows = match q.bind(limit).fetch_all(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "body_html": r.body_html,
                "created_at": r.created_at,
                "author": { "username": r.username, "domain": r.domain },
            })
        })
        .collect();
    let next_cursor = if rows.len() == limit as usize {
        rows.last()
            .map(|last| format!("{}-{}", last.created_at.timestamp(), last.id))
    } else {
        None
    };

    Json(json!({ "items": items, "next_cursor": next_cursor })).into_response()
}

#[derive(Debug, Deserialize)]
pub struct CreateCommentBody {
    pub body: String,
}

/// POST /api/v1/clips/{id}/comments — sanitized create: HTML is stripped to
/// plain text before storage and length checks. Bumps the clip's atomic
/// comment counter and notifies the clip author.
///
/// Comment bodies are scanned for hashtags but not linked: only clips have a
/// link table (`clip_hashtags`), so tags in comments would be orphans.
pub async fn create_comment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(payload): Json<CreateCommentBody>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let clip = match fetch_live_clip(pool, id).await {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if clip.comments_disabled {
        return problem(
            StatusCode::FORBIDDEN,
            "comments disabled",
            format!("clip {id} does not accept comments"),
        );
    }

    let text = strip_html_tags(&payload.body);
    let text = text.trim();
    if text.is_empty() {
        return problem(
            StatusCode::BAD_REQUEST,
            "empty comment",
            "comment body is required",
        );
    }
    if text.chars().count() > COMMENT_MAX_CHARS {
        return problem(
            StatusCode::BAD_REQUEST,
            "comment too long",
            format!("comment body must be at most {COMMENT_MAX_CHARS} characters"),
        );
    }

    // Unique placeholder ap_id; canonicalized once the row id exists below.
    let placeholder = format!("https://toottok.local/comments/{}", Uuid::new_v4());
    let comment = match Comment::create(pool, id, auth.actor.id, None, &placeholder, text).await {
        Ok(c) => c,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    let ap_id = format!("{}/comments/{}", state.cfg.public_base_url(), comment.id);
    if let Err(e) = Comment::set_ap_id(pool, comment.id, &ap_id).await {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }

    if let Err(e) = sqlx::query("UPDATE clips SET comment_count = comment_count + 1 WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await
    {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }

    if clip.actor_id != auth.actor.id {
        if let Err(e) = sqlx::query(
            "INSERT INTO notifications (actor_id, kind, source_actor_id, clip_id) \
             VALUES ($1, 'comment', $2, $3)",
        )
        .bind(clip.actor_id)
        .bind(auth.actor.id)
        .bind(id)
        .execute(pool)
        .await
        {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    }

    (
        StatusCode::CREATED,
        Json(json!({ "id": comment.id, "body_html": comment.body_html })),
    )
        .into_response()
}

/// DELETE /api/v1/comments/{id} — author-or-admin soft delete; the parent
/// clip's counter is floored at zero.
pub async fn delete_comment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let comment = match Comment::fetch_by_id(pool, id).await {
        Ok(Some(c)) if c.deleted_at.is_none() => c,
        Ok(_) => {
            return problem(
                StatusCode::NOT_FOUND,
                "comment not found",
                format!("no live comment with id {id}"),
            )
        }
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    if comment.actor_id != auth.actor.id && !auth.user.is_admin {
        return problem(
            StatusCode::FORBIDDEN,
            "forbidden",
            "only the comment author or an admin may delete a comment",
        );
    }

    let removed = match sqlx::query(
        "UPDATE comments SET deleted_at = now(), body_html = '[deleted]' \
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .execute(pool)
    .await
    {
        Ok(res) => res.rows_affected(),
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    if removed > 0 {
        if let Err(e) = sqlx::query(
            "UPDATE clips SET comment_count = GREATEST(comment_count - 1, 0) WHERE id = $1",
        )
        .bind(comment.clip_id)
        .execute(pool)
        .await
        {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    }

    (StatusCode::OK, Json(json!({ "deleted": true, "id": id }))).into_response()
}

// ------------------------------------------------------------- notifications

/// One notification row joined with its source actor handle.
#[derive(Debug, sqlx::FromRow)]
struct NotificationRow {
    id: i64,
    kind: String,
    source_username: String,
    source_avatar_path: Option<String>,
    clip_id: Option<i64>,
    read_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

/// GET /api/v1/notifications — own rows newest-first, default 30 per page.
pub async fn list_notifications(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<FeedParams>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let (before_created_at, before_id) = match cursor_keyset(&params) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let limit = params
        .page
        .map(|p| p as i64)
        .unwrap_or(NOTIFICATIONS_DEFAULT_LIMIT)
        .clamp(1, NOTIFICATIONS_MAX_LIMIT);

    let mut sql = String::from(
        "SELECT n.id, n.kind, a.username AS source_username, \
         a.avatar_path AS source_avatar_path, n.clip_id, n.read_at, n.created_at \
         FROM notifications n JOIN actors a ON a.id = n.source_actor_id \
         WHERE n.actor_id = $1",
    );
    if before_created_at.is_some() {
        sql.push_str(" AND (n.created_at, n.id) < ($2, $3)");
    }
    // The LIMIT placeholder index depends on whether the cursor bound $2/$3;
    // referencing a higher parameter than the highest bound one makes
    // Postgres unable to infer its type.
    let limit_param = if before_created_at.is_some() {
        "$4"
    } else {
        "$2"
    };
    sql.push_str(&format!(
        " ORDER BY n.created_at DESC, n.id DESC LIMIT {limit_param}"
    ));

    let mut q = sqlx::query_as::<_, NotificationRow>(&sql).bind(auth.actor.id);
    if let (Some(ts), Some(cid)) = (before_created_at, before_id) {
        q = q.bind(ts).bind(cid);
    }
    let rows = match q.bind(limit).fetch_all(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "kind": r.kind,
                "source": {
                    "username": r.source_username,
                    "avatar_path": r.source_avatar_path,
                },
                "clip_id": r.clip_id,
                "read_at": r.read_at,
                "created_at": r.created_at,
            })
        })
        .collect();
    let next_cursor = if rows.len() == limit as usize {
        rows.last()
            .map(|last| format!("{}-{}", last.created_at.timestamp(), last.id))
    } else {
        None
    };

    Json(json!({ "items": items, "next_cursor": next_cursor })).into_response()
}

#[derive(Debug, Deserialize)]
pub struct MarkReadBody {
    pub ids: Option<Vec<i64>>,
}

/// PUT /api/v1/notifications/read — stamp read_at on the given ids, or on all
/// unread rows when `ids` is absent/empty.
pub async fn mark_read(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<MarkReadBody>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };

    let result = match body.ids.as_deref() {
        Some(ids) if !ids.is_empty() => {
            sqlx::query(
                "UPDATE notifications SET read_at = now() \
                 WHERE actor_id = $1 AND id = ANY($2) AND read_at IS NULL",
            )
            .bind(auth.actor.id)
            .bind(ids)
            .execute(pool)
            .await
        }
        _ => {
            sqlx::query(
                "UPDATE notifications SET read_at = now() \
                 WHERE actor_id = $1 AND read_at IS NULL",
            )
            .bind(auth.actor.id)
            .execute(pool)
            .await
        }
    };

    match result {
        Ok(res) => (
            StatusCode::OK,
            Json(json!({ "updated": res.rows_affected() })),
        )
            .into_response(),
        Err(e) => problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        ),
    }
}

// -------------------------------------------------------------------- search

#[derive(Debug, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct ActorSearchRow {
    username: String,
    display_name: Option<String>,
    domain: Option<String>,
    avatar_path: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct ClipSearchRow {
    id: i64,
    caption_html: Option<String>,
}

/// GET /api/v1/search?q=&type= — public prefix matches over actor handles /
/// display names and hashtags plus substring caption search. `type`
/// (`actors`|`tags`|`clips`) restricts which sections are returned.
pub async fn search(State(state): State<AppState>, Query(params): Query<SearchParams>) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let q = params.q.as_deref().unwrap_or("").trim();
    if q.chars().count() < 2 {
        return problem(
            StatusCode::BAD_REQUEST,
            "invalid query",
            "q is required and must be at least 2 characters",
        );
    }
    let want = |section: &str| params.kind.as_deref().is_none_or(|k| k == section);
    if let Some(k) = params.kind.as_deref() {
        if !matches!(k, "actors" | "tags" | "clips") {
            return problem(
                StatusCode::BAD_REQUEST,
                "invalid type",
                "type must be one of actors, tags, clips",
            );
        }
    }

    let mut out = Map::new();

    if want("actors") {
        let prefix = format!("{q}%");
        match sqlx::query_as::<_, ActorSearchRow>(
            "SELECT username, display_name, domain, avatar_path FROM actors \
             WHERE (username ILIKE $1 OR display_name ILIKE $1) \
             AND deleted_at IS NULL AND suspended_at IS NULL LIMIT 10",
        )
        .bind(&prefix)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => {
                let mut actors: Vec<Value> = rows
                    .iter()
                    .map(|a| {
                        json!({
                            "username": a.username,
                            "display_name": a.display_name,
                            "domain": a.domain,
                            "avatar_path": a.avatar_path,
                        })
                    })
                    .collect();
                // No cached hit — try resolving a remote handle
                // (`user@domain` / `@user@domain`) via WebFinger, like
                // Mastodon/Akkoma do at query time. The resolved actor is
                // fetched + cached, so the follow button works immediately.
                if actors.is_empty() && (q.contains('@')) {
                    let handle = q.trim().trim_start_matches('@');
                    match toottok_federation::resolve_remote_actor_by_handle(
                        pool,
                        &state.egress,
                        handle,
                    )
                    .await
                    {
                        Ok(Some(a)) => {
                            actors.push(json!({
                                "username": a.username,
                                "display_name": a.display_name,
                                "domain": a.domain,
                                "avatar_path": a.avatar_path,
                            }));
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "remote actor resolve failed");
                        }
                    }
                }
                out.insert("actors".into(), Value::Array(actors));
            }
            Err(e) => {
                return problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database error",
                    format!("{e}"),
                )
            }
        }
    }

    if want("tags") {
        let tag_prefix = format!("{}%", q.trim_start_matches('#'));
        match sqlx::query_scalar::<_, String>(
            "SELECT tag FROM hashtags WHERE tag ILIKE $1 LIMIT 10",
        )
        .bind(&tag_prefix)
        .fetch_all(pool)
        .await
        {
            Ok(tags) => {
                out.insert(
                    "tags".into(),
                    Value::Array(tags.into_iter().map(Value::String).collect()),
                );
            }
            Err(e) => {
                return problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database error",
                    format!("{e}"),
                )
            }
        }
    }

    if want("clips") {
        let contains = format!("%{q}%");
        match sqlx::query_as::<_, ClipSearchRow>(
            "SELECT id, caption_html FROM clips \
             WHERE caption_html ILIKE $1 AND deleted_at IS NULL \
             AND visibility = 'public' AND status = 'ready' LIMIT 20",
        )
        .bind(&contains)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => {
                let clips: Vec<Value> = rows
                    .iter()
                    .map(|c| json!({ "id": c.id, "caption_html": c.caption_html }))
                    .collect();
                out.insert("clips".into(), Value::Array(clips));
            }
            Err(e) => {
                return problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database error",
                    format!("{e}"),
                )
            }
        }
    }

    Json(Value::Object(out)).into_response()
}

// ------------------------------------------------------------------- reports

#[derive(Debug, Deserialize)]
pub struct CreateReportBody {
    pub target_type: String,
    pub target_id: i64,
    pub category: Option<String>,
    pub body: Option<String>,
}

/// POST /api/v1/reports — file a moderation report against a clip, comment,
/// or actor. `category` is free-form (`reports.category` carries no CHECK).
pub async fn create_report(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(payload): Json<CreateReportBody>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if !matches!(payload.target_type.as_str(), "clip" | "comment" | "actor") {
        return problem(
            StatusCode::BAD_REQUEST,
            "invalid target_type",
            "target_type must be one of clip, comment, actor",
        );
    }
    let detail = match payload.body.as_deref() {
        None => "",
        Some(b) if b.chars().count() <= REPORT_BODY_MAX_CHARS => b,
        Some(_) => {
            return problem(
                StatusCode::BAD_REQUEST,
                "report too long",
                format!("report body must be at most {REPORT_BODY_MAX_CHARS} characters"),
            )
        }
    };
    let category = payload
        .category
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());

    let id: i64 = match sqlx::query_scalar(
        "INSERT INTO reports (reporter_actor_id, target_type, target_id, category, body) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(auth.actor.id)
    .bind(&payload.target_type)
    .bind(payload.target_id)
    .bind(category)
    .bind(detail)
    .fetch_one(pool)
    .await
    {
        Ok(id) => id,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    (
        StatusCode::CREATED,
        Json(json!({
            "id": id,
            "target_type": payload.target_type,
            "target_id": payload.target_id,
            "state": "open",
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct AdminReportsParams {
    pub state: Option<String>,
}

/// One admin report row with reporter handle and a one-line target summary.
#[derive(Debug, sqlx::FromRow)]
struct AdminReportRow {
    id: i64,
    target_type: String,
    target_id: i64,
    category: Option<String>,
    body: String,
    state: String,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
    reporter_username: Option<String>,
    target_summary: Option<String>,
}

#[allow(clippy::result_large_err)]
fn require_admin(auth: &AuthUser) -> Result<(), Response> {
    if auth.user.is_admin {
        Ok(())
    } else {
        Err(problem(
            StatusCode::FORBIDDEN,
            "forbidden",
            "admin privileges are required",
        ))
    }
}

/// GET /api/v1/admin/reports?state=open|resolved|rejected|all — newest-first
/// admin queue (defaults to `open`), capped at 100 rows.
pub async fn admin_reports(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<AdminReportsParams>,
) -> Response {
    if let Err(resp) = require_admin(&auth) {
        return resp;
    }
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let state_filter = params.state.as_deref().unwrap_or("open");
    if !matches!(state_filter, "open" | "resolved" | "rejected" | "all") {
        return problem(
            StatusCode::BAD_REQUEST,
            "invalid state",
            "state must be one of open, resolved, rejected, all",
        );
    }

    let mut sql = String::from(
        "SELECT r.id, r.target_type, r.target_id, r.category, r.body, r.state, \
         r.created_at, r.resolved_at, ra.username AS reporter_username, \
         CASE r.target_type \
           WHEN 'clip' THEN (SELECT c.caption_html FROM clips c WHERE c.id = r.target_id) \
           WHEN 'comment' THEN (SELECT cm.body_html FROM comments cm WHERE cm.id = r.target_id) \
           ELSE (SELECT ac.username FROM actors ac WHERE ac.id = r.target_id) \
         END AS target_summary \
         FROM reports r LEFT JOIN actors ra ON ra.id = r.reporter_actor_id",
    );
    if state_filter != "all" {
        sql.push_str(" WHERE r.state = $1");
    }
    sql.push_str(" ORDER BY r.id DESC LIMIT 100");

    let mut q = sqlx::query_as::<_, AdminReportRow>(&sql);
    if state_filter != "all" {
        q = q.bind(state_filter);
    }
    let rows = match q.fetch_all(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "target_type": r.target_type,
                "target_id": r.target_id,
                "category": r.category,
                "body": r.body,
                "state": r.state,
                "created_at": r.created_at,
                "resolved_at": r.resolved_at,
                "reporter": { "username": r.reporter_username },
                "target_summary": r.target_summary,
            })
        })
        .collect();

    Json(json!({ "items": items })).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ResolveReportBody {
    pub action_note: Option<String>,
}

/// POST /api/v1/admin/reports/{id}/resolve — close an open report. The schema
/// has no dedicated note column, so a non-empty `action_note` is appended to
/// the report's `body`.
pub async fn resolve_report(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
    Json(body): Json<ResolveReportBody>,
) -> Response {
    if let Err(resp) = require_admin(&auth) {
        return resp;
    }
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let note = body.action_note.as_deref().unwrap_or("").trim();

    let resolved_at: Option<DateTime<Utc>> = match sqlx::query_scalar(
        r#"
        UPDATE reports
        SET state = 'resolved', resolved_at = now(),
            body = CASE
              WHEN $2 = '' THEN body
              WHEN body = '' THEN $2
              ELSE body || E'\n' || $2
            END
        WHERE id = $1 AND state = 'open'
        RETURNING resolved_at
        "#,
    )
    .bind(id)
    .bind(note)
    .fetch_optional(pool)
    .await
    {
        Ok(resolved_at) => resolved_at,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    if resolved_at.is_none() {
        return problem(
            StatusCode::NOT_FOUND,
            "report not found",
            format!("no open report with id {id}"),
        );
    }

    if let Err(e) = toottok_db::audit::log(
        pool,
        auth.actor.id,
        "reports.resolve",
        "report",
        Some(id),
        &json!({ "action_note": note }),
    )
    .await
    {
        tracing::error!(error = %e, action = "reports.resolve", "audit_log write failed");
    }

    (
        StatusCode::OK,
        Json(json!({ "id": id, "state": "resolved", "resolved_at": resolved_at })),
    )
        .into_response()
}

// ---------------------------------------------------------------- profiles

#[derive(Debug, Deserialize)]
pub struct ProfileParams {
    pub domain: Option<String>,
    pub cursor: Option<String>,
    pub page: Option<usize>,
}

#[derive(Debug, sqlx::FromRow)]
struct ProfileActorRow {
    id: i64,
    username: String,
    display_name: Option<String>,
    domain: Option<String>,
    avatar_path: Option<String>,
    summary: Option<String>,
}

/// GET /api/v1/profiles/{username}?domain=&cursor= — public profile card plus
/// the profile owner's public clips, feed-card shaped, newest-first keyset.
pub async fn profile_grid(
    State(state): State<AppState>,
    auth: OptionalAuthUser,
    Path(username): Path<String>,
    Query(params): Query<ProfileParams>,
) -> Response {
    let viewer = auth.0;
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let (before_created_at, before_id) = match cursor_keyset(&FeedParams {
        cursor: params.cursor.clone(),
        page: params.page,
    }) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let limit = feed_limit(&FeedParams {
        cursor: None,
        page: params.page,
    });

    let mut sql =
        String::from("SELECT id, username, display_name, domain, avatar_path, summary FROM actors WHERE username = $1");
    if params.domain.is_some() {
        sql.push_str(" AND domain = $2");
    } else {
        sql.push_str(" AND domain IS NULL");
    }
    sql.push_str(" AND deleted_at IS NULL AND suspended_at IS NULL");

    let mut q = sqlx::query_as::<_, ProfileActorRow>(&sql).bind(&username);
    if let Some(domain) = &params.domain {
        q = q.bind(domain);
    }
    let actor = match q.fetch_optional(pool).await {
        Ok(Some(actor)) => actor,
        Ok(None) => {
            return problem(
                StatusCode::NOT_FOUND,
                "profile not found",
                format!("no actor named @{username}"),
            )
        }
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    let mut sql = String::from(
        "SELECT c.id, c.ap_id, c.caption_html, c.duration_s, c.width, c.height, \
         c.like_count, c.comment_count, c.share_count, c.created_at AS clip_created_at, \
         c.actor_id, a.username, a.display_name, a.avatar_path, a.domain, \
         c.remote_media_url \
         FROM clips c JOIN actors a ON a.id = c.actor_id \
         WHERE c.deleted_at IS NULL AND c.visibility = 'public' \
         AND (c.status = 'ready' OR c.origin = 'remote') AND c.actor_id = $1",
    );
    if before_created_at.is_some() {
        sql.push_str(" AND (c.created_at, c.id) < ($2, $3)");
    }
    // Same placeholder-index rule as the comments listing above.
    let grid_limit_param = if before_created_at.is_some() {
        "$4"
    } else {
        "$2"
    };
    sql.push_str(&format!(
        " ORDER BY c.created_at DESC, c.id DESC LIMIT {grid_limit_param}"
    ));

    let mut q = sqlx::query_as::<_, FeedClipRow>(&sql).bind(actor.id);
    if let (Some(ts), Some(cid)) = (before_created_at, before_id) {
        q = q.bind(ts).bind(cid);
    }
    let rows = match q.bind(limit).fetch_all(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    let items = match feed_rows_to_values(pool, &rows).await {
        Ok(items) => items,
        Err(resp) => return resp,
    };
    let next_cursor = if rows.len() == limit as usize {
        rows.last()
            .map(|last| format!("{}-{}", last.clip_created_at.timestamp(), last.id))
    } else {
        None
    };

    let follower_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM follows WHERE target_actor_id = $1 AND state = 'accepted'",
    )
    .bind(actor.id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let following_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM follows WHERE follower_actor_id = $1 AND state = 'accepted'",
    )
    .bind(actor.id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let total_likes: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(like_count), 0) FROM clips \
         WHERE actor_id = $1 AND deleted_at IS NULL",
    )
    .bind(actor.id)
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let viewer_following = if let Some(viewer) = viewer.as_ref() {
        Follow::fetch_by_pair(pool, viewer.actor.id, actor.id)
            .await
            .ok()
            .flatten()
            .map(|f| f.state == "accepted")
            .unwrap_or(false)
    } else {
        false
    };

    Json(json!({
        "actor": {
            "actor_id": actor.id,
            "username": actor.username,
            "display_name": actor.display_name,
            "domain": actor.domain,
            "avatar_path": actor.avatar_path,
            "summary": actor.summary,
        },
        "follower_count": follower_count,
        "following_count": following_count,
        "likes_received": total_likes,
        "is_following": viewer_following,
        "clips": items,
        "next_cursor": next_cursor,
    }))
    .into_response()
}
