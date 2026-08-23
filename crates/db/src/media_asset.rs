use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::error::DbError;

/// One stored media object for a LOCAL clip (media_assets row). `ready_at` is
/// stamped by the finalize job; rows inserted by transcode start unready.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct MediaAsset {
    pub id: i64,
    pub clip_id: i64,
    pub kind: String,
    pub rendition: String,
    pub lang: Option<String>,
    pub path: String,
    pub mime: String,
    pub size_bytes: Option<i64>,
    pub bitrate_kbps: Option<i32>,
    pub codec: Option<String>,
    pub ready_at: Option<DateTime<Utc>>,
}

impl MediaAsset {
    pub async fn fetch_for_clip(pool: &sqlx::PgPool, clip_id: i64) -> Result<Vec<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM media_assets WHERE clip_id = $1 ORDER BY id")
                .bind(clip_id)
                .fetch_all(pool)
                .await?,
        )
    }

    /// Filename of the LARGEST ready public mp4 rendition for a clip
    /// (mega-review F5): sub-720p sources must not federate a 720.mp4 that
    /// was never produced. Prefers 1080 > 720 > 480 > orig among
    /// kind='video_mp4' assets with ready_at set. `None` when nothing is
    /// ready yet (callers fall back to `720.mp4` naming or omit the URL).
    pub async fn largest_video_filename(
        pool: &sqlx::PgPool,
        clip_id: i64,
    ) -> Result<Option<String>, DbError> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT rendition FROM media_assets \
             WHERE clip_id = $1 AND kind = 'video_mp4' AND ready_at IS NOT NULL",
        )
        .bind(clip_id)
        .fetch_all(pool)
        .await?;
        for want in ["1080", "720", "480"] {
            if rows.iter().any(|r| r == want) {
                return Ok(Some(format!("{want}.mp4")));
            }
        }
        if rows.iter().any(|r| r == "orig") {
            // Original keeps its uuid filename; caller should resolve via the
            // asset row instead of guessing. Report None so callers fall back.
            return Ok(None);
        }
        Ok(None)
    }

    /// Locate the asset for a clip whose storage key ends in `filename`.
    pub async fn find_for_clip_filename(
        pool: &sqlx::PgPool,
        clip_id: i64,
        filename: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(Self::fetch_for_clip(pool, clip_id)
            .await?
            .into_iter()
            .find(|a| a.path.rsplit('/').next() == Some(filename)))
    }

    /// Insert an asset row (unready). The transcode job inserts the ladder
    /// rungs, the poster, and an `orig` row pointing at the original file so
    /// the source stays locatable after finalize.
    ///
    /// Idempotent per `(clip_id, kind, rendition)` (unique index from
    /// migration 0003): on conflict the existing row is returned untouched,
    /// so a transcode re-run never duplicates asset rows.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        pool: &sqlx::PgPool,
        clip_id: i64,
        kind: &str,
        rendition: &str,
        path: &str,
        mime: &str,
        size_bytes: Option<i64>,
        bitrate_kbps: Option<i32>,
        codec: Option<&str>,
    ) -> Result<Self, DbError> {
        let created = sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO media_assets (clip_id, kind, rendition, path, mime, size_bytes, bitrate_kbps, codec)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (clip_id, kind, rendition) DO NOTHING
            RETURNING *
            "#,
        )
        .bind(clip_id)
        .bind(kind)
        .bind(rendition)
        .bind(path)
        .bind(mime)
        .bind(size_bytes)
        .bind(bitrate_kbps)
        .bind(codec)
        .fetch_optional(pool)
        .await?;

        match created {
            Some(asset) => Ok(asset),
            None => Ok(sqlx::query_as::<_, Self>(
                "SELECT * FROM media_assets WHERE clip_id = $1 AND kind = $2 AND rendition = $3",
            )
            .bind(clip_id)
            .bind(kind)
            .bind(rendition)
            .fetch_one(pool)
            .await?),
        }
    }

    /// Insert an asset row, updating in place when the `(clip_id, kind,
    /// rendition)` row already exists (e.g. the `orig` row registered early by
    /// the upload path and refreshed by the transcode job). Idempotent under
    /// the migration-0003 unique index; the transcode re-run never duplicates
    /// the orig row and never clobbers `ready_at` (finalize owns that stamp).
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert(
        pool: &sqlx::PgPool,
        clip_id: i64,
        kind: &str,
        rendition: &str,
        path: &str,
        mime: &str,
        size_bytes: Option<i64>,
        bitrate_kbps: Option<i32>,
        codec: Option<&str>,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO media_assets (clip_id, kind, rendition, path, mime, size_bytes, bitrate_kbps, codec)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (clip_id, kind, rendition) DO UPDATE SET
                path = EXCLUDED.path,
                mime = EXCLUDED.mime,
                size_bytes = COALESCE(EXCLUDED.size_bytes, media_assets.size_bytes),
                bitrate_kbps = COALESCE(EXCLUDED.bitrate_kbps, media_assets.bitrate_kbps),
                codec = COALESCE(EXCLUDED.codec, media_assets.codec)
            RETURNING *
            "#,
        )
        .bind(clip_id)
        .bind(kind)
        .bind(rendition)
        .bind(path)
        .bind(mime)
        .bind(size_bytes)
        .bind(bitrate_kbps)
        .bind(codec)
        .fetch_one(pool)
        .await?)
    }

    /// Finalize step: stamp every not-yet-ready asset for a clip.
    pub async fn mark_ready_for_clip(pool: &sqlx::PgPool, clip_id: i64) -> Result<(), DbError> {
        sqlx::query(
            r#"
            UPDATE media_assets
            SET ready_at = now()
            WHERE clip_id = $1 AND ready_at IS NULL
            "#,
        )
        .bind(clip_id)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Cleanup on a failed transcode: drop any partial asset rows.
    pub async fn delete_for_clip(pool: &sqlx::PgPool, clip_id: i64) -> Result<(), DbError> {
        sqlx::query("DELETE FROM media_assets WHERE clip_id = $1")
            .bind(clip_id)
            .execute(pool)
            .await?;
        Ok(())
    }
}
