use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Like {
    pub clip_id: i64,
    pub actor_id: i64,
    pub ap_activity_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Like {
    pub async fn fetch_by_pair(
        pool: &sqlx::PgPool,
        clip_id: i64,
        actor_id: i64,
    ) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM likes WHERE clip_id = $1 AND actor_id = $2")
                .bind(clip_id)
                .bind(actor_id)
                .fetch_optional(pool)
                .await?,
        )
    }

    pub async fn create(
        pool: &sqlx::PgPool,
        clip_id: i64,
        actor_id: i64,
        ap_activity_id: Option<&str>,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO likes (clip_id, actor_id, ap_activity_id)
            VALUES ($1, $2, $3)
            RETURNING *
            "#,
        )
        .bind(clip_id)
        .bind(actor_id)
        .bind(ap_activity_id)
        .fetch_one(pool)
        .await?)
    }
}
