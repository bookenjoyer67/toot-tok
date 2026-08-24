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
    pub share_count: i64,
    pub clip_created_at: DateTime<Utc>,
    pub actor_id: i64,
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
         c.like_count, c.comment_count, c.share_count, c.created_at AS clip_created_at, \
         c.actor_id, a.username, a.display_name, a.avatar_path, a.domain, \
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

/// Newest-first public clips from LOCAL actors only (`actors.domain IS NULL`)
/// — the fediverse "local timeline", same keyset pagination as
/// [`discover_feed`].
pub async fn local_feed(
    pool: &sqlx::PgPool,
    before_created_at: Option<DateTime<Utc>>,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<FeedClipRow>, DbError> {
    feed_page_local(pool, before_created_at, before_id, limit).await
}

/// Shared page fetcher for the local timeline. Identical to [`feed_page`]
/// without the follow join, plus the `a.domain IS NULL` local-actor filter.
async fn feed_page_local(
    pool: &sqlx::PgPool,
    before_created_at: Option<DateTime<Utc>>,
    before_id: Option<i64>,
    limit: i64,
) -> Result<Vec<FeedClipRow>, DbError> {
    let has_cursor = before_created_at.is_some() && before_id.is_some();

    let mut sql = String::from(
        "SELECT c.id, c.ap_id, c.caption_html, c.duration_s, c.width, c.height, \
         c.like_count, c.comment_count, c.share_count, c.created_at AS clip_created_at, \
         c.actor_id, a.username, a.display_name, a.avatar_path, a.domain, \
         c.remote_media_url \
         FROM clips c JOIN actors a ON a.id = c.actor_id \
         WHERE c.deleted_at IS NULL AND c.visibility = 'public' \
         AND a.suspended_at IS NULL AND a.deleted_at IS NULL \
         AND (c.status = 'ready' OR c.origin = 'remote') \
         AND a.domain IS NULL",
    );

    if has_cursor {
        sql.push_str(" AND (c.created_at, c.id) < ($1, $2)");
    }
    sql.push_str(&format!(
        " ORDER BY c.created_at DESC, c.id DESC LIMIT ${}",
        if has_cursor { 3 } else { 1 }
    ));

    let mut q = sqlx::query_as::<_, FeedClipRow>(&sql);
    if has_cursor {
        q = q.bind(before_created_at).bind(before_id);
    }
    Ok(q.bind(limit).fetch_all(pool).await?)
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
         c.like_count, c.comment_count, c.share_count, c.created_at AS clip_created_at, \
         c.actor_id, a.username, a.display_name, a.avatar_path, a.domain, \
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

/// Trending clips: engagement-weighted, recency-decayed. Score =
/// (likes + 3*comments + 5*shares) / hours_since_post^1.2. Simple v1
/// ranking — no view counters exist yet. Returns feed-card rows.
pub async fn trending(
    pool: &sqlx::PgPool,
    limit: i64,
) -> Result<Vec<FeedClipRow>, DbError> {
    Ok(sqlx::query_as::<_, FeedClipRow>(
        r#"
        SELECT c.id, c.ap_id, c.caption_html, c.duration_s, c.width, c.height,
               c.like_count, c.comment_count, c.share_count,
               c.created_at AS clip_created_at,
               c.actor_id, a.username, a.display_name, a.avatar_path, a.domain,
               c.remote_media_url
        FROM clips c
        JOIN actors a ON a.id = c.actor_id
        WHERE c.deleted_at IS NULL AND c.visibility = 'public'
          AND a.suspended_at IS NULL AND a.deleted_at IS NULL
          AND (c.status = 'ready' OR c.origin = 'remote')
          AND c.created_at > now() - interval '30 days'
        ORDER BY POWER(
                    (c.like_count + 3 * c.comment_count + 5 * c.share_count)::double precision
                    / POWER(GREATEST(EXTRACT(EPOCH FROM (now() - c.created_at)) / 3600.0, 0.25), 1.2),
                    1) DESC,
                 c.created_at DESC
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

/// Trending hashtags: most-tagged public clips over the last 14 days.
pub async fn trending_tags(
    pool: &sqlx::PgPool,
    limit: i64,
) -> Result<Vec<(String, i64)>, DbError> {
    Ok(sqlx::query_as(
        r#"
        SELECT h.tag, COUNT(*) AS uses
        FROM clip_hashtags ch
        JOIN hashtags h ON h.id = ch.hashtag_id
        JOIN clips c ON c.id = ch.clip_id
        WHERE c.deleted_at IS NULL AND c.visibility = 'public'
          AND c.created_at > now() - interval '14 days'
        GROUP BY h.tag
        ORDER BY uses DESC, h.tag ASC
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?)
}
