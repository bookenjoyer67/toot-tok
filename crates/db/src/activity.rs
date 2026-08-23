use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Activity {
    pub id: i64,
    pub activity_id: String,
    pub direction: String,
    pub actor_ap_id: String,
    pub object_ap_id: Option<String>,
    pub raw: Value,
    pub received_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
}

impl Activity {
    pub async fn fetch_by_id(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM activities WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?,
        )
    }

    pub async fn create_inbound(
        pool: &sqlx::PgPool,
        activity_id: &str,
        actor_ap_id: &str,
        object_ap_id: Option<&str>,
        raw: &Value,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO activities (activity_id, direction, actor_ap_id, object_ap_id, raw)
            VALUES ($1, 'inbound', $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(activity_id)
        .bind(actor_ap_id)
        .bind(object_ap_id)
        .bind(raw)
        .fetch_one(pool)
        .await?)
    }

    /// The idempotency gate for the inbound pipeline (ARCHITECTURE §4): insert
    /// with `ON CONFLICT DO NOTHING`. `Some(activity)` when this was a fresh
    /// delivery (row inserted), `None` when `activity_id` was already processed
    /// — the caller must skip processing.
    pub async fn try_create_inbound(
        pool: &sqlx::PgPool,
        activity_id: &str,
        actor_ap_id: &str,
        object_ap_id: Option<&str>,
        raw: &Value,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO activities (activity_id, direction, actor_ap_id, object_ap_id, raw)
            VALUES ($1, 'inbound', $2, $3, $4)
            ON CONFLICT (activity_id) DO NOTHING
            RETURNING *
            "#,
        )
        .bind(activity_id)
        .bind(actor_ap_id)
        .bind(object_ap_id)
        .bind(raw)
        .fetch_optional(pool)
        .await?)
    }

    /// Log an outbound activity (signed + POSTed by a worker).
    pub async fn create_outbound(
        pool: &sqlx::PgPool,
        activity_id: &str,
        actor_ap_id: &str,
        object_ap_id: Option<&str>,
        raw: &Value,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO activities (activity_id, direction, actor_ap_id, object_ap_id, raw)
            VALUES ($1, 'outbound', $2, $3, $4)
            ON CONFLICT (activity_id) DO NOTHING
            RETURNING *
            "#,
        )
        .bind(activity_id)
        .bind(actor_ap_id)
        .bind(object_ap_id)
        .bind(raw)
        .fetch_one(pool)
        .await?)
    }

    /// Stamp `processed_at` after a successful inbound pipeline run.
    pub async fn stamp_processed(
        pool: &sqlx::PgPool,
        activity_id: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE activities
            SET processed_at = now()
            WHERE activity_id = $1 AND processed_at IS NULL
            RETURNING *
            "#,
        )
        .bind(activity_id)
        .fetch_optional(pool)
        .await?)
    }
}
