use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::error::DbError;

/// One row of the `instances` table: per-domain federation bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Instance {
    pub domain: String,
    pub software: Option<String>,
    pub version: Option<String>,
    pub inbox_url: String,
    pub disabled_at: Option<DateTime<Utc>>,
    pub failure_count: i32,
    pub last_success_at: Option<DateTime<Utc>>,
}

impl Instance {
    /// Upsert on any successful remote contact: refresh software/version/inbox,
    /// stamp `last_success_at` and reset `failure_count`.
    pub async fn upsert_success(
        pool: &sqlx::PgPool,
        domain: &str,
        software: Option<&str>,
        version: Option<&str>,
        inbox_url: &str,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO instances (domain, software, version, inbox_url, failure_count, last_success_at)
            VALUES ($1, $2, $3, $4, 0, now())
            ON CONFLICT (domain) DO UPDATE
            SET software = EXCLUDED.software,
                version = EXCLUDED.version,
                inbox_url = EXCLUDED.inbox_url,
                failure_count = 0,
                last_success_at = now()
            RETURNING *
            "#,
        )
        .bind(domain)
        .bind(software)
        .bind(version)
        .bind(inbox_url)
        .fetch_one(pool)
        .await?)
    }

    /// Upsert on a failed remote contact: ensure the row exists and increment
    /// `failure_count`. Success counters are left untouched.
    pub async fn record_failure(
        pool: &sqlx::PgPool,
        domain: &str,
        inbox_url: &str,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO instances (domain, inbox_url, failure_count)
            VALUES ($1, $2, 1)
            ON CONFLICT (domain) DO UPDATE
            SET failure_count = instances.failure_count + 1
            RETURNING *
            "#,
        )
        .bind(domain)
        .bind(inbox_url)
        .fetch_one(pool)
        .await?)
    }
}
