use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Setting {
    pub key: String,
    pub value: Value,
    pub updated_at: DateTime<Utc>,
}

impl Setting {
    pub async fn fetch_by_key(pool: &sqlx::PgPool, key: &str) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM settings WHERE key = $1")
                .bind(key)
                .fetch_optional(pool)
                .await?,
        )
    }

    pub async fn set(pool: &sqlx::PgPool, key: &str, value: &Value) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO settings (key, value)
            VALUES ($1, $2)
            ON CONFLICT (key) DO UPDATE
                SET value = EXCLUDED.value, updated_at = now()
            RETURNING *
            "#,
        )
        .bind(key)
        .bind(value)
        .fetch_one(pool)
        .await?)
    }
}
