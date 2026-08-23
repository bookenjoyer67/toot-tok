use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::error::DbError;

/// One row of the `tombstones` table — the delete-wins gate for federated
/// objects. A tombstone for an `ap_id` swallows later `Create`s of that object.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Tombstone {
    pub ap_id: String,
    pub r#type: String,
    pub deleted_at: DateTime<Utc>,
}

impl Tombstone {
    pub async fn upsert(pool: &sqlx::PgPool, ap_id: &str, r#type: &str) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO tombstones (ap_id, type)
            VALUES ($1, $2)
            ON CONFLICT (ap_id) DO UPDATE
            SET type = EXCLUDED.type
            RETURNING *
            "#,
        )
        .bind(ap_id)
        .bind(r#type)
        .fetch_one(pool)
        .await?)
    }

    pub async fn exists(pool: &sqlx::PgPool, ap_id: &str) -> Result<bool, DbError> {
        Ok(
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tombstones WHERE ap_id = $1)")
                .bind(ap_id)
                .fetch_one(pool)
                .await?,
        )
    }
}
