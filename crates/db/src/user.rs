use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct User {
    pub id: i64,
    pub actor_id: i64,
    pub email: Option<String>,
    pub email_verified_at: Option<DateTime<Utc>>,
    /// NULLable since migration 0006: account erasure NULLs it out; deleted
    /// users must not be able to authenticate anyway.
    pub password_hash: Option<String>,
    pub totp_secret: Option<String>,
    pub totp_recovery_codes: Option<Value>,
    pub is_admin: bool,
    /// `active` (immediate signup) or `pending` (approval-mode signup awaiting
    /// admin approval).
    pub status: String,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl User {
    pub async fn fetch_by_id(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM users WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?,
        )
    }

    pub async fn fetch_by_actor_id(
        pool: &sqlx::PgPool,
        actor_id: i64,
    ) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM users WHERE actor_id = $1")
                .bind(actor_id)
                .fetch_optional(pool)
                .await?,
        )
    }

    pub async fn fetch_by_email(pool: &sqlx::PgPool, email: &str) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM users WHERE email = $1")
                .bind(email)
                .fetch_optional(pool)
                .await?,
        )
    }

    pub async fn create(
        pool: &sqlx::PgPool,
        actor_id: i64,
        email: Option<&str>,
        password_hash: &str,
    ) -> Result<Self, DbError> {
        Self::create_with_status(pool, actor_id, email, password_hash, "active").await
    }

    /// Create a user with an explicit account status (approval-mode signups
    /// land as `pending`).
    pub async fn create_with_status(
        pool: &sqlx::PgPool,
        actor_id: i64,
        email: Option<&str>,
        password_hash: &str,
        status: &str,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO users (actor_id, email, password_hash, status)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(actor_id)
        .bind(email)
        .bind(password_hash)
        .bind(status)
        .fetch_one(pool)
        .await?)
    }

    /// Create a user with `is_admin` set, used by the `create-admin` CLI.
    pub async fn create_admin(
        pool: &sqlx::PgPool,
        actor_id: i64,
        email: Option<&str>,
        password_hash: &str,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO users (actor_id, email, password_hash, is_admin)
            VALUES ($1, $2, $3, TRUE)
            RETURNING *
            "#,
        )
        .bind(actor_id)
        .bind(email)
        .bind(password_hash)
        .fetch_one(pool)
        .await?)
    }

    /// POST /api/v1/auth/verify-email — stamp `email_verified_at`.
    pub async fn set_email_verified(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE users
            SET email_verified_at = now()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(pool)
        .await?)
    }

    /// POST /api/v1/auth/reset — replace the argon2id password hash.
    pub async fn set_password(
        pool: &sqlx::PgPool,
        id: i64,
        password_hash: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE users
            SET password_hash = $2
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(password_hash)
        .fetch_optional(pool)
        .await?)
    }

    /// Admin approve/suspend-adjacent status transitions (`active`/`pending`).
    pub async fn set_status(
        pool: &sqlx::PgPool,
        id: i64,
        status: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE users
            SET status = $2
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(status)
        .fetch_optional(pool)
        .await?)
    }

    /// Account deletion (local half): stamp `deleted_at` and NULL out the
    /// personal/credential columns (email, password hash, TOTP secrets) and
    /// drop the admin flag.
    pub async fn mark_deleted_and_erase(
        pool: &sqlx::PgPool,
        id: i64,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE users
            SET deleted_at = now(),
                email = NULL,
                password_hash = NULL,
                totp_secret = NULL,
                totp_recovery_codes = NULL,
                is_admin = FALSE
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(pool)
        .await?)
    }
}
