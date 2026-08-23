//! Keyset-paginated feed queries (Phase 6).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;

use crate::error::DbError;

/// One feed row: clip columns plus the author fields needed to render a card.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct FeedClipRow {
    pub id: i64,
    pub ap_id: String,
    pub caption_html: Option<String>,
    pub duration_s: Option<f64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub like_count: i64,
    pub comment_count: i64,
    pub clip_created_at: DateTime<Utc>,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_path: Option<String>,
    pub domain: Option<String>,
    pub remote_media_url: Option<String>,
}

/// Shared page fetcher. With a viewer id, restricts to clips from actors the
/// viewer follows (accepted follows); without one, returns the global
/// discover feed. Cursor is the `(created_at, id)` keyset tuple; pass both
/// halves or neither.
async fn feed_page(
    pool: &sqlx::PgPool,
    viewer_actor_id: Option<i64>,
    before_created_at: Option<DateTime<Utc>>,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<FeedClipRow>, DbError> {
    let has_viewer = viewer_actor_id.is_some();
    let has_cursor = before_created_at.is_some() && before_id.is_some();

    let mut sql = String::from(
        "SELECT c.id, c.ap_id, c.caption_html, c.duration_s, c.width, c.height, \
         c.like_count, c.comment_count, c.created_at AS clip_created_at, \
         a.username, a.display_name, a.avatar_path, a.domain, \
         c.remote_media_url \
         FROM clips c JOIN actors a ON a.id = c.actor_id",
    );
    if has_viewer {
        sql.push_str(" JOIN follows f ON f.target_actor_id = c.actor_id");
    }
    sql.push_str(
        " WHERE c.deleted_at IS NULL AND c.visibility = 'public' \
         AND a.suspended_at IS NULL AND a.deleted_at IS NULL \
         AND (c.status = 'ready' OR c.origin = 'remote')",
    );

    let mut next_param = 1;
    if has_viewer {
        sql.push_str(&format!(
            " AND f.follower_actor_id = ${next_param} AND f.state = 'accepted'"
        ));
        next_param += 1;
    }
    if has_cursor {
        sql.push_str(&format!(
            " AND (c.created_at, c.id) < (${next_param}, ${})",
            next_param + 1
        ));
        next_param += 2;
    }
    sql.push_str(&format!(
        " ORDER BY c.created_at DESC, c.id DESC LIMIT ${next_param}"
    ));

    let mut q = sqlx::query_as::<_, FeedClipRow>(&sql);
    if let Some(viewer) = viewer_actor_id {
        q = q.bind(viewer);
    }
    if has_cursor {
        q = q.bind(before_created_at).bind(before_id);
    }
    Ok(q.bind(limit).fetch_all(pool).await?)
}

/// Newest-first clips from actors `viewer_actor_id` accepted-follows,
/// paginating with the `(before_created_at, before_id)` keyset cursor.
pub async fn following_feed(
    pool: &sqlx::PgPool,
    viewer_actor_id: i64,
    before_created_at: Option<DateTime<Utc>>,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<FeedClipRow>, DbError> {
    feed_page(
        pool,
        Some(viewer_actor_id),
        before_created_at,
        before_id,
        limit,
    )
    .await
}

/// Newest-first public clips across all actors (no follow join), same keyset
/// pagination as [`following_feed`].
pub async fn discover_feed(
    pool: &sqlx::PgPool,
    before_created_at: Option<DateTime<Utc>>,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<FeedClipRow>, DbError> {
    feed_page(pool, None, before_created_at, before_id, limit).await
}

/// Newest-first public clips tagged `tag` (citext match, input lowercased),
/// same keyset pagination as [`discover_feed`]. Feeds GET
/// /api/v1/tags/{tag}/clips.
pub async fn clips_by_tag(
    pool: &sqlx::PgPool,
    tag: &str,
    before_created_at: Option<DateTime<Utc>>,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<FeedClipRow>, DbError> {
    let has_cursor = before_created_at.is_some() && before_id.is_some();
    let mut sql = String::from(
        "SELECT c.id, c.ap_id, c.caption_html, c.duration_s, c.width, c.height, \
         c.like_count, c.comment_count, c.created_at AS clip_created_at, \
         a.username, a.display_name, a.avatar_path, a.domain, \
         c.remote_media_url \
         FROM clips c \
         JOIN actors a ON a.id = c.actor_id \
         JOIN clip_hashtags ch ON ch.clip_id = c.id \
         JOIN hashtags h ON h.id = ch.hashtag_id \
         WHERE h.tag = $1::text \
         AND c.deleted_at IS NULL AND c.visibility = 'public' \
         AND a.suspended_at IS NULL AND a.deleted_at IS NULL \
         AND (c.status = 'ready' OR c.origin = 'remote')",
    );
    if has_cursor {
        // Cursor params are $2/$3 only when present; the LIMIT placeholder is
        // numbered accordingly below (Postgres rejects gaps in $n usage).
        sql.push_str(
            " AND (c.created_at, c.id) < ($2::timestamptz, $3::bigint)",
        );
        sql.push_str(" ORDER BY c.created_at DESC, c.id DESC LIMIT $4::bigint");
    } else {
        sql.push_str(" ORDER BY c.created_at DESC, c.id DESC LIMIT $2::bigint");
    }

    let mut q = sqlx::query_as::<_, FeedClipRow>(&sql).bind(tag.to_lowercase());
    if has_cursor {
        q = q.bind(before_created_at).bind(before_id);
    }
    Ok(q.bind(limit).fetch_all(pool).await?)
}
