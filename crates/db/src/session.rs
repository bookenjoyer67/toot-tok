use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::error::DbError;

/// A server-side login session. `id` is the hex SHA-256 of the opaque cookie
/// token (never stored in plaintext). `csrf_token` is the per-session value the
/// client must echo back in the `X-Toottok-CSRF` header on state-changing
/// requests (ARCHITECTURE.md §8).
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Session {
    pub id: String,
    pub user_id: i64,
    pub expires_at: DateTime<Utc>,
    pub ip: Option<String>,
    pub ua: Option<String>,
    pub csrf_token: String,
}

impl Session {
    pub async fn create(
        pool: &sqlx::PgPool,
        id: &str,
        user_id: i64,
        expires_at: DateTime<Utc>,
        ip: Option<&str>,
        ua: Option<&str>,
        csrf_token: &str,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO sessions (id, user_id, expires_at, ip, ua, csrf_token)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(user_id)
        .bind(expires_at)
        .bind(ip)
        .bind(ua)
        .bind(csrf_token)
        .fetch_one(pool)
        .await?)
    }

    pub async fn fetch_by_id(pool: &sqlx::PgPool, id: &str) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM sessions WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?,
        )
    }

    pub async fn delete_by_id(pool: &sqlx::PgPool, id: &str) -> Result<(), DbError> {
        sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Revoke every session belonging to a user (logout-everywhere, password
    /// reset, account deletion).
    pub async fn delete_for_user(pool: &sqlx::PgPool, user_id: i64) -> Result<(), DbError> {
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user_id)
            .execute(pool)
            .await?;
        Ok(())
    }
}
