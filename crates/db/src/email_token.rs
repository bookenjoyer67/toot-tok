use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::error::DbError;
use crate::user::User;

/// One outstanding email-bound token: email verification (`verify`) or
/// password reset (`password_reset`). Only the SHA-256 hash of the raw token
/// is stored; tokens are single-use and TTL-bounded.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct EmailToken {
    pub id: i64,
    pub user_id: i64,
    pub kind: String,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl EmailToken {
    pub async fn create(
        pool: &sqlx::PgPool,
        user_id: i64,
        kind: &str,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO email_tokens (user_id, kind, token_hash, expires_at)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(user_id)
        .bind(kind)
        .bind(token_hash)
        .bind(expires_at)
        .fetch_one(pool)
        .await?)
    }

    /// Atomically consume an unexpired, unused token of `kind`, returning the
    /// owning user. `None` when the token is unknown, expired, or already used
    /// (single-use guarantee via the `used_at IS NULL` guard in the UPDATE).
    pub async fn consume(
        pool: &sqlx::PgPool,
        token_hash: &str,
        kind: &str,
    ) -> Result<Option<User>, DbError> {
        let user_id = sqlx::query_scalar::<_, i64>(
            r#"
            UPDATE email_tokens
            SET used_at = now()
            WHERE token_hash = $1 AND kind = $2 AND used_at IS NULL AND expires_at > now()
            RETURNING user_id
            "#,
        )
        .bind(token_hash)
        .bind(kind)
        .fetch_optional(pool)
        .await?;

        match user_id {
            Some(uid) => Ok(User::fetch_by_id(pool, uid).await?),
            None => Ok(None),
        }
    }

    /// Invalidate every outstanding token of `kind` for a user (a successful
    /// password reset kills all earlier reset links).
    pub async fn invalidate_for_user(
        pool: &sqlx::PgPool,
        user_id: i64,
        kind: &str,
    ) -> Result<(), DbError> {
        sqlx::query(
            r#"
            UPDATE email_tokens
            SET used_at = now()
            WHERE user_id = $1 AND kind = $2 AND used_at IS NULL
            "#,
        )
        .bind(user_id)
        .bind(kind)
        .execute(pool)
        .await?;
        Ok(())
    }

    pub async fn delete_for_user(pool: &sqlx::PgPool, user_id: i64) -> Result<(), DbError> {
        sqlx::query("DELETE FROM email_tokens WHERE user_id = $1")
            .bind(user_id)
            .execute(pool)
            .await?;
        Ok(())
    }
}
