use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Clip {
    pub id: i64,
    pub actor_id: i64,
    pub ap_id: String,
    pub origin: String,
    pub caption_html: Option<String>,
    pub visibility: String,
    pub status: String,
    pub duration_s: Option<f64>,
    pub sha256_hash: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub size_bytes: Option<i64>,
    pub remote_media_url: Option<String>,
    pub remote_poster_url: Option<String>,
    pub is_sensitive: bool,
    pub cw_text: Option<String>,
    pub comments_disabled: bool,
    pub downloads_allowed: bool,
    pub like_count: i64,
    pub comment_count: i64,
    pub share_count: i64,
    pub view_count: i64,
    pub published_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Clip {
    pub async fn fetch_by_id(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM clips WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?,
        )
    }

    /// Local-upload dedup: find a live, non-failed clip already stored with
    /// this hash. Failed clips are excluded so a rejected upload's hash can be
    /// re-uploaded (mirrors the partial unique index in migration 0003).
    pub async fn fetch_by_sha256(pool: &sqlx::PgPool, hash: &str) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            SELECT * FROM clips
            WHERE sha256_hash = $1 AND deleted_at IS NULL AND status <> 'failed'
            ORDER BY id
            LIMIT 1
            "#,
        )
        .bind(hash)
        .fetch_optional(pool)
        .await?)
    }

    /// Fetch a clip by its canonical ActivityPub object id.
    pub async fn fetch_by_ap_id(pool: &sqlx::PgPool, ap_id: &str) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM clips WHERE ap_id = $1")
                .bind(ap_id)
                .fetch_optional(pool)
                .await?,
        )
    }

    /// Point a local clip's `ap_id` at its canonical URI
    /// (`{base}/clips/{id}`). Done once at finalize, before the federation
    /// `Create` is built (the placeholder upload-time ap_id is not routable).
    pub async fn set_ap_id(pool: &sqlx::PgPool, id: i64, ap_id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE clips SET ap_id = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(ap_id)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Attach (or clear, with None) the sound reference on a clip.
    pub async fn set_sound(
        pool: &sqlx::PgPool,
        clip_id: i64,
        sound_id: Option<i64>,
    ) -> Result<(), DbError> {
        sqlx::query("UPDATE clips SET sound_id = $2, updated_at = now() WHERE id = $1")
            .bind(clip_id)
            .bind(sound_id)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Federated/moderation delete: tombstone timestamp + status flip. The row
    /// is kept (delete-wins gate for later Creates).
    pub async fn mark_deleted(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE clips
            SET deleted_at = now(), status = 'deleted', updated_at = now()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(pool)
        .await?)
    }

    /// Insert a freshly uploaded local clip awaiting the probe job.
    pub async fn create_pending_upload(
        pool: &sqlx::PgPool,
        actor_id: i64,
        ap_id: &str,
        sha256_hash: &str,
        size_bytes: i64,
        caption_html: Option<&str>,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO clips (actor_id, ap_id, origin, caption_html, status, sha256_hash, size_bytes)
            VALUES ($1, $2, 'local', $3, 'pending', $4, $5)
            RETURNING *
            "#
        )
        .bind(actor_id)
        .bind(ap_id)
        .bind(caption_html)
        .bind(sha256_hash)
        .bind(size_bytes)
        .bind(actor_id)
        .bind(ap_id)
        .bind(sha256_hash)
        .bind(size_bytes)
        .fetch_one(pool)
        .await?)
    }

    /// Stamp probe results on a successful probe and move the clip into the
    /// pipeline (`pending -> processing`); transcode/finalize advances it to
    /// `ready`.
    pub async fn update_probe_info(
        pool: &sqlx::PgPool,
        id: i64,
        duration_s: Option<f64>,
        width: Option<i32>,
        height: Option<i32>,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE clips
            SET status = 'processing', duration_s = $2, width = $3, height = $4
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(duration_s)
        .bind(width)
        .bind(height)
        .fetch_optional(pool)
        .await?)
    }

    /// REJECT path: over-cap duration or undecodable media.
    pub async fn mark_failed(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE clips
            SET status = 'failed'
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(pool)
        .await?)
    }

    /// Finalize step: ladder done, assets stamped, clip is playable.
    pub async fn mark_ready(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE clips
            SET status = 'ready', published_at = COALESCE(published_at, now())
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(pool)
        .await?)
    }

    pub async fn create_local(
        pool: &sqlx::PgPool,
        actor_id: i64,
        ap_id: &str,
        caption_html: Option<&str>,
        visibility: &str,
        status: &str,
        published_at: Option<DateTime<Utc>>,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO clips (actor_id, ap_id, origin, caption_html, visibility, status, published_at)
            VALUES ($1, $2, 'local', $3, $4, $5, $6)
            RETURNING *
            "#,
        )
        .bind(actor_id)
        .bind(ap_id)
        .bind(caption_html)
        .bind(visibility)
        .bind(status)
        .bind(published_at)
        .fetch_one(pool)
        .await?)
    }

    /// Insert a federated clip arriving via Create(Note)+video attachment.
    /// Remote rows are born `ready` (ARCHITECTURE §3): they are never
    /// probed or transcoded; media stays at `remote_media_url`. `sha256`
    /// stays NULL (dedup is local-uploads only).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_remote(
        pool: &sqlx::PgPool,
        actor_id: i64,
        ap_id: &str,
        caption_html: Option<&str>,
        duration_s: Option<f64>,
        width: Option<i32>,
        height: Option<i32>,
        remote_media_url: &str,
        is_sensitive: bool,
        cw_text: Option<&str>,
        published_at: Option<DateTime<Utc>>,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO clips (actor_id, ap_id, origin, caption_html, status, duration_s,
                               width, height, remote_media_url, is_sensitive, cw_text, published_at)
            VALUES ($1, $2, 'remote', $3, 'ready', $4, $5, $6, $7, $8, $9, $10)
            RETURNING *
            "#,
        )
        .bind(actor_id)
        .bind(ap_id)
        .bind(caption_html)
        .bind(duration_s)
        .bind(width)
        .bind(height)
        .bind(remote_media_url)
        .bind(is_sensitive)
        .bind(cw_text)
        .bind(published_at)
        .fetch_one(pool)
        .await?)
    }

    /// Inbound Update(Note) on a known clip: refresh caption / sensitivity /
    /// CW text. Ownership is checked by the caller.
    pub async fn update_note_fields(
        pool: &sqlx::PgPool,
        id: i64,
        caption_html: Option<&str>,
        is_sensitive: bool,
        cw_text: Option<&str>,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE clips
            SET caption_html = $2, is_sensitive = $3, cw_text = $4, updated_at = now()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(caption_html)
        .bind(is_sensitive)
        .bind(cw_text)
        .fetch_optional(pool)
        .await?)
    }
}
