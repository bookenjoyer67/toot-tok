//! Bookmarks: private per-viewer saved-clips list. Local-only — no AP
//! activity, no notifications, no counters.

use sqlx::PgPool;

use crate::error::DbError;
use crate::feed::FeedClipRow;

/// Insert a bookmark; idempotent. Returns true when the row was newly added.
pub async fn add(pool: &PgPool, clip_id: i64, actor_id: i64) -> Result<bool, DbError> {
    let res =
        sqlx::query("INSERT INTO bookmarks (clip_id, actor_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(clip_id)
            .bind(actor_id)
            .execute(pool)
            .await?;
    Ok(res.rows_affected() > 0)
}

/// Remove a bookmark. Returns true when a row was deleted.
pub async fn remove(pool: &PgPool, clip_id: i64, actor_id: i64) -> Result<bool, DbError> {
    let res = sqlx::query("DELETE FROM bookmarks WHERE clip_id = $1 AND actor_id = $2")
        .bind(clip_id)
        .bind(actor_id)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Does this viewer hold the bookmark?
pub async fn exists(pool: &PgPool, clip_id: i64, actor_id: i64) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM bookmarks WHERE clip_id = $1 AND actor_id = $2",
    )
    .bind(clip_id)
    .bind(actor_id)
    .fetch_one(pool)
    .await?
        > 0)
}

/// The viewer's saved clips, feed-card shaped, newest-bookmark first.
/// Keyset cursor over `bookmarks.created_at` mirrors the clip feeds.
pub async fn list(
    pool: &PgPool,
    actor_id: i64,
    before_created_at: Option<chrono::DateTime<chrono::Utc>>,
    before_clip_id: Option<i64>,
    limit: i64,
) -> Result<Vec<FeedClipRow>, DbError> {
    let has_cursor = before_created_at.is_some() && before_clip_id.is_some();
    let mut sql = String::from(
        "SELECT c.id, c.ap_id, c.caption_html, c.duration_s, c.width, c.height, \
         c.like_count, c.comment_count, c.share_count, \
         c.created_at AS clip_created_at, \
         c.actor_id, a.username, a.display_name, a.avatar_path, a.domain, \
         c.remote_media_url \
         FROM bookmarks b \
         JOIN clips c ON c.id = b.clip_id \
         JOIN actors a ON a.id = c.actor_id \
         WHERE b.actor_id = $1 AND c.deleted_at IS NULL \
         AND (c.status = 'ready' OR c.origin = 'remote')",
    );
    if has_cursor {
        sql.push_str(" AND (b.created_at, b.clip_id) < ($2, $3)");
        sql.push_str(" ORDER BY b.created_at DESC, b.clip_id DESC LIMIT $4");
    } else {
        // Postgres rejects gaps in $n usage: number the LIMIT accordingly.
        sql.push_str(" ORDER BY b.created_at DESC, b.clip_id DESC LIMIT $2");
    }

    let mut q = sqlx::query_as::<_, FeedClipRow>(&sql).bind(actor_id);
    if has_cursor {
        q = q.bind(before_created_at).bind(before_clip_id);
    }
    Ok(q.bind(limit).fetch_all(pool).await?)
}
