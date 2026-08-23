use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use crate::actor::Actor;
use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Follow {
    pub follower_actor_id: i64,
    pub target_actor_id: i64,
    pub ap_activity_id: Option<String>,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Follow {
    pub async fn fetch_by_pair(
        pool: &sqlx::PgPool,
        follower_actor_id: i64,
        target_actor_id: i64,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            "SELECT * FROM follows WHERE follower_actor_id = $1 AND target_actor_id = $2",
        )
        .bind(follower_actor_id)
        .bind(target_actor_id)
        .fetch_optional(pool)
        .await?)
    }

    pub async fn create(
        pool: &sqlx::PgPool,
        follower_actor_id: i64,
        target_actor_id: i64,
        ap_activity_id: Option<&str>,
        state: &str,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO follows (follower_actor_id, target_actor_id, ap_activity_id, state)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(follower_actor_id)
        .bind(target_actor_id)
        .bind(ap_activity_id)
        .bind(state)
        .fetch_one(pool)
        .await?)
    }

    /// Upsert a follow relation (outbound or inbound) and set its state. Used
    /// when a remote actor follows a local one (state `requested`), when a
    /// follow is accepted/rejected, and when a local user follows a remote one.
    pub async fn upsert(
        pool: &sqlx::PgPool,
        follower_actor_id: i64,
        target_actor_id: i64,
        ap_activity_id: Option<&str>,
        state: &str,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO follows (follower_actor_id, target_actor_id, ap_activity_id, state)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (follower_actor_id, target_actor_id) DO UPDATE
            SET ap_activity_id = COALESCE(EXCLUDED.ap_activity_id, follows.ap_activity_id),
                state = EXCLUDED.state
            RETURNING *
            "#,
        )
        .bind(follower_actor_id)
        .bind(target_actor_id)
        .bind(ap_activity_id)
        .bind(state)
        .fetch_one(pool)
        .await?)
    }

    /// Flip the state of an existing follow (e.g. `requested` → `accepted`
    /// when the Accept arrives). No-op when the pair does not exist.
    pub async fn set_state(
        pool: &sqlx::PgPool,
        follower_actor_id: i64,
        target_actor_id: i64,
        state: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE follows
            SET state = $3
            WHERE follower_actor_id = $1 AND target_actor_id = $2
            RETURNING *
            "#,
        )
        .bind(follower_actor_id)
        .bind(target_actor_id)
        .bind(state)
        .fetch_optional(pool)
        .await?)
    }

    /// Find a follow row by its originating ActivityPub activity id (the `id`
    /// of the Follow activity that created it).
    pub async fn fetch_by_activity_id(
        pool: &sqlx::PgPool,
        ap_activity_id: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM follows WHERE ap_activity_id = $1")
                .bind(ap_activity_id)
                .fetch_optional(pool)
                .await?,
        )
    }

    /// Drop a follow relation entirely (Undo(Follow) / unfollow). `true` when a
    /// row was removed.
    pub async fn delete(
        pool: &sqlx::PgPool,
        follower_actor_id: i64,
        target_actor_id: i64,
    ) -> Result<bool, DbError> {
        let res = sqlx::query(
            "DELETE FROM follows WHERE follower_actor_id = $1 AND target_actor_id = $2",
        )
        .bind(follower_actor_id)
        .bind(target_actor_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// All follower actor ids of a target (for outbox/collections and
    /// followers-visible delivery fan-out).
    pub async fn follower_actor_ids(
        pool: &sqlx::PgPool,
        target_actor_id: i64,
    ) -> Result<Vec<i64>, DbError> {
        Ok(sqlx::query_scalar(
            "SELECT follower_actor_id FROM follows WHERE target_actor_id = $1 AND state = 'accepted'",
        )
        .bind(target_actor_id)
        .fetch_all(pool)
        .await?)
    }

    /// All target actor ids a follower follows (following collection).
    pub async fn following_actor_ids(
        pool: &sqlx::PgPool,
        follower_actor_id: i64,
    ) -> Result<Vec<i64>, DbError> {
        Ok(
            sqlx::query_scalar("SELECT target_actor_id FROM follows WHERE follower_actor_id = $1")
                .bind(follower_actor_id)
                .fetch_all(pool)
                .await?,
        )
    }

    /// Remote (cached, `domain IS NOT NULL`) actors with an accepted follow of
    /// `target_actor_id` — the shared-inbox fan-out targets when the author
    /// publishes a clip.
    pub async fn remote_follower_actors(
        pool: &sqlx::PgPool,
        target_actor_id: i64,
    ) -> Result<Vec<Actor>, DbError> {
        Ok(sqlx::query_as::<_, Actor>(
            r#"
            SELECT a.*
            FROM follows f
            JOIN actors a ON a.id = f.follower_actor_id
            WHERE f.target_actor_id = $1 AND f.state = 'accepted' AND a.domain IS NOT NULL
            ORDER BY a.id
            "#,
        )
        .bind(target_actor_id)
        .fetch_all(pool)
        .await?)
    }
}
