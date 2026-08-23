use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Comment {
    pub id: i64,
    pub clip_id: i64,
    pub actor_id: i64,
    pub parent_comment_id: Option<i64>,
    pub ap_id: String,
    pub body_html: String,
    pub like_count: i64,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl Comment {
    pub async fn fetch_by_id(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM comments WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?,
        )
    }

    pub async fn create(
        pool: &sqlx::PgPool,
        clip_id: i64,
        actor_id: i64,
        parent_comment_id: Option<i64>,
        ap_id: &str,
        body_html: &str,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO comments (clip_id, actor_id, parent_comment_id, ap_id, body_html)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
        )
        .bind(clip_id)
        .bind(actor_id)
        .bind(parent_comment_id)
        .bind(ap_id)
        .bind(body_html)
        .fetch_one(pool)
        .await?)
    }

    /// Point a comment's `ap_id` at its canonical URI (`{base}/comments/{id}`).
    /// Done right after insert; the insert-time value is a unique placeholder
    /// because the row id is not known yet.
    pub async fn set_ap_id(pool: &sqlx::PgPool, id: i64, ap_id: &str) -> Result<(), DbError> {
        sqlx::query("UPDATE comments SET ap_id = $2 WHERE id = $1")
            .bind(id)
            .bind(ap_id)
            .execute(pool)
            .await?;
        Ok(())
    }
}
