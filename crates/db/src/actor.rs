use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Actor {
    pub id: i64,
    pub username: String,
    pub domain: Option<String>,
    pub actor_type: String,
    pub public_key_pem: String,
    pub private_key_pem: Option<String>,
    pub inbox_url: String,
    pub shared_inbox_url: Option<String>,
    pub outbox_url: String,
    pub followers_url: String,
    pub display_name: Option<String>,
    pub summary: Option<String>,
    pub avatar_path: Option<String>,
    pub header_path: Option<String>,
    pub manually_approves_followers: bool,
    pub is_locked: bool,
    pub suspended_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub ap_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Actor {
    pub async fn fetch_by_id(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM actors WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?,
        )
    }

    pub async fn fetch_by_ap_id(pool: &sqlx::PgPool, ap_id: &str) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM actors WHERE ap_id = $1")
                .bind(ap_id)
                .fetch_optional(pool)
                .await?,
        )
    }

    /// Fetch a LOCAL actor by username. `username` is CITEXT so the lookup is
    /// case-insensitive; the partial unique index guarantees at most one row.
    pub async fn fetch_by_username_local(
        pool: &sqlx::PgPool,
        username: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>(
                "SELECT * FROM actors WHERE username = $1 AND domain IS NULL",
            )
            .bind(username)
            .fetch_optional(pool)
            .await?,
        )
    }

    /// Update the profile fields editable via PATCH /api/v1/accounts/me.
    pub async fn update_profile(
        pool: &sqlx::PgPool,
        id: i64,
        display_name: Option<&str>,
        summary: Option<&str>,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE actors
            SET display_name = $2, summary = $3
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(display_name)
        .bind(summary)
        .fetch_optional(pool)
        .await?)
    }

    pub async fn set_avatar_path(
        pool: &sqlx::PgPool,
        id: i64,
        avatar_path: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE actors
            SET avatar_path = $2
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(avatar_path)
        .fetch_optional(pool)
        .await?)
    }

    /// Moderation: mark a local actor suspended (`actors.suspended_at`).
    pub async fn suspend(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE actors
            SET suspended_at = now()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(pool)
        .await?)
    }

    /// Self-deletion tombstone: `actors.deleted_at`.
    pub async fn mark_deleted(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE actors
            SET deleted_at = now()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(pool)
        .await?)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create(
        pool: &sqlx::PgPool,
        username: &str,
        domain: Option<&str>,
        actor_type: &str,
        public_key_pem: &str,
        private_key_pem: Option<&str>,
        inbox_url: &str,
        shared_inbox_url: Option<&str>,
        outbox_url: &str,
        followers_url: &str,
        ap_id: &str,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO actors
                (username, domain, actor_type, public_key_pem, private_key_pem,
                 inbox_url, shared_inbox_url, outbox_url, followers_url, ap_id)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            RETURNING *
            "#,
        )
        .bind(username)
        .bind(domain)
        .bind(actor_type)
        .bind(public_key_pem)
        .bind(private_key_pem)
        .bind(inbox_url)
        .bind(shared_inbox_url)
        .bind(outbox_url)
        .bind(followers_url)
        .bind(ap_id)
        .fetch_one(pool)
        .await?)
    }

    /// Create a remote (cached) actor or refresh an existing row from a fresh
    /// fetch. `username` may be reused across domains (no partial unique index
    /// applies because `domain` is non-NULL); `updated_at` is stamped so the
    /// crate's stale-refresh logic (`last_refreshed_at`) sees the fetch.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_remote(
        pool: &sqlx::PgPool,
        username: &str,
        domain: &str,
        actor_type: &str,
        public_key_pem: &str,
        inbox_url: &str,
        shared_inbox_url: Option<&str>,
        outbox_url: &str,
        followers_url: &str,
        ap_id: &str,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO actors
                (username, domain, actor_type, public_key_pem, private_key_pem,
                 inbox_url, shared_inbox_url, outbox_url, followers_url, ap_id)
            VALUES ($1, $2, $3, $4, NULL, $5, $6, $7, $8, $9)
            ON CONFLICT (ap_id) DO UPDATE
            SET username = EXCLUDED.username,
                domain = EXCLUDED.domain,
                actor_type = EXCLUDED.actor_type,
                public_key_pem = EXCLUDED.public_key_pem,
                inbox_url = EXCLUDED.inbox_url,
                shared_inbox_url = EXCLUDED.shared_inbox_url,
                outbox_url = EXCLUDED.outbox_url,
                followers_url = EXCLUDED.followers_url,
                deleted_at = NULL,
                updated_at = now()
            RETURNING *
            "#,
        )
        .bind(username)
        .bind(domain)
        .bind(actor_type)
        .bind(public_key_pem)
        .bind(inbox_url)
        .bind(shared_inbox_url)
        .bind(outbox_url)
        .bind(followers_url)
        .bind(ap_id)
        .fetch_one(pool)
        .await?)
    }

    /// The local instance actor (`Application` at `/ap/actor`): create it on
    /// first boot when absent, otherwise return the existing row untouched
    /// (keys are never regenerated once persisted).
    #[allow(clippy::too_many_arguments)]
    pub async fn ensure_instance_actor(
        pool: &sqlx::PgPool,
        username: &str,
        public_key_pem: &str,
        private_key_pem: &str,
        ap_id: &str,
        inbox_url: &str,
        outbox_url: &str,
        followers_url: &str,
    ) -> Result<Self, DbError> {
        let inserted = sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO actors
                (username, domain, actor_type, public_key_pem, private_key_pem,
                 inbox_url, shared_inbox_url, outbox_url, followers_url, ap_id)
            VALUES ($1, NULL, 'application', $2, $3, $4, $4, $5, $6, $7)
            ON CONFLICT (ap_id) DO NOTHING
            RETURNING *
            "#,
        )
        .bind(username)
        .bind(public_key_pem)
        .bind(private_key_pem)
        .bind(inbox_url)
        .bind(outbox_url)
        .bind(followers_url)
        .bind(ap_id)
        .fetch_optional(pool)
        .await?;
        match inserted {
            Some(actor) => Ok(actor),
            None => Ok(Self::fetch_by_ap_id(pool, ap_id)
                .await?
                .expect("instance actor exists after conflict")),
        }
    }

    /// Set `deleted_at` for a remote actor row when its Delete(Person) arrives.
    pub async fn mark_remote_deleted_by_ap_id(
        pool: &sqlx::PgPool,
        ap_id: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE actors
            SET deleted_at = now()
            WHERE ap_id = $1
            RETURNING *
            "#,
        )
        .bind(ap_id)
        .fetch_optional(pool)
        .await?)
    }
}
