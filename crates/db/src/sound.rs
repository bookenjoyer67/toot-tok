//! Sounds: named audio attribution shared across clips. V1 stores the NAME
//! only (no audio bytes): "original sound — @alice" or a user-typed track.
//! Clips sharing a `sound_id` group onto one sound page.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::DbError;

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct Sound {
    pub id: i64,
    pub title: String,
    pub author_actor_id: Option<i64>,
    pub author_username: Option<String>,
    pub clip_count: i64,
    pub created_at: DateTime<Utc>,
}

const SOUND_COLS: &str =
    "s.id, s.title, s.author_actor_id, a.username AS author_username, \
     (SELECT COUNT(*) FROM clips c WHERE c.sound_id = s.id AND c.deleted_at IS NULL) AS clip_count, \
     s.created_at";

/// Find-or-create a sound by (title, owner). Owner NULL = unattributed track.
/// Idempotent via the unique index; safe under concurrent uploads.
pub async fn get_or_create(
    pool: &PgPool,
    title: &str,
    author_actor_id: Option<i64>,
) -> Result<i64, DbError> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO sounds (title, author_actor_id) VALUES ($1::text, $2::bigint) \
         ON CONFLICT (title, author_actor_id) DO UPDATE SET title = EXCLUDED.title \
         RETURNING id",
    )
    .bind(title)
    .bind(author_actor_id)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

/// One sound row by id.
pub async fn fetch(pool: &PgPool, id: i64) -> Result<Option<Sound>, DbError> {
    Ok(sqlx::query_as::<_, Sound>(
        &format!(
            "SELECT {SOUND_COLS} FROM sounds s \
             LEFT JOIN actors a ON a.id = s.author_actor_id WHERE s.id = $1"
        ),
    )
    .bind(id)
    .fetch_optional(pool)
    .await?)
}

/// Clips using this sound, feed-card shaped (same row shape as the feeds so
/// the server can reuse render_feed). Newest first.
pub async fn clips_for_sound(
    pool: &PgPool,
    sound_id: i64,
    before_created_at: Option<DateTime<Utc>>,
    before_clip_id: Option<i64>,
    limit: i64,
) -> Result<Vec<crate::feed::FeedClipRow>, DbError> {
    let has_cursor = before_created_at.is_some() && before_clip_id.is_some();
    let mut sql = String::from(
        "SELECT c.id, c.ap_id, c.caption_html, c.duration_s, c.width, c.height, \
         c.like_count, c.comment_count, c.share_count, \
         c.created_at AS clip_created_at, \
         c.actor_id, a.username, a.display_name, a.avatar_path, a.domain, \
         c.remote_media_url \
         FROM clips c JOIN actors a ON a.id = c.actor_id \
         WHERE c.sound_id = $1 AND c.deleted_at IS NULL \
         AND (c.status = 'ready' OR c.origin = 'remote')",
    );
    if has_cursor {
        sql.push_str(" AND (c.created_at, c.id) < ($2, $3)");
        sql.push_str(" ORDER BY c.created_at DESC, c.id DESC LIMIT $4");
    } else {
        // PG rejects gaps in $n usage — renumber LIMIT when no cursor.
        sql.push_str(" ORDER BY c.created_at DESC, c.id DESC LIMIT $2");
    }

    let mut q = sqlx::query_as::<_, crate::feed::FeedClipRow>(&sql).bind(sound_id);
    if has_cursor {
        q = q.bind(before_created_at).bind(before_clip_id);
    }
    Ok(q.bind(limit).fetch_all(pool).await?)
}
