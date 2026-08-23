use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;

use crate::error::DbError;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub payload: Value,
    pub run_after: DateTime<Utc>,
    pub attempts: i32,
    pub max_attempts: i32,
    pub last_error: Option<String>,
    pub state: String,
    pub locked_by: Option<String>,
    pub locked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl Job {
    pub async fn fetch_by_id(pool: &sqlx::PgPool, id: i64) -> Result<Option<Self>, DbError> {
        Ok(
            sqlx::query_as::<_, Self>("SELECT * FROM jobs WHERE id = $1")
                .bind(id)
                .fetch_optional(pool)
                .await?,
        )
    }

    pub async fn create(
        pool: &sqlx::PgPool,
        kind: &str,
        payload: &Value,
        run_after: Option<DateTime<Utc>>,
    ) -> Result<Self, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            INSERT INTO jobs (kind, payload, run_after)
            VALUES ($1, $2, $3)
            RETURNING *
            "#,
        )
        .bind(kind)
        .bind(payload)
        .bind(run_after.unwrap_or_else(Utc::now))
        .fetch_one(pool)
        .await?)
    }

    /// Claim the oldest due job, atomically skipping rows another worker has
    /// already locked. Marks the claimed job `running` under `worker`.
    ///
    /// The lock, the queued-row pick and the state flip happen in a single
    /// statement, so the row lock is never dropped between selection and
    /// update the way a `SELECT ... FOR UPDATE` followed by a separate
    /// autocommit `UPDATE` would be.
    pub async fn claim_next_due(
        pool: &sqlx::PgPool,
        worker: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(CLAIM_NEXT_DUE_SQL)
            .bind(worker)
            .fetch_optional(pool)
            .await?)
    }

    /// Same as [`Self::claim_next_due`], but runs inside the caller's
    /// transaction so the picked row stays locked until the caller commits.
    pub async fn claim_next_due_tx(
        tx: &mut sqlx::PgTransaction<'_>,
        worker: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(CLAIM_NEXT_DUE_SQL)
            .bind(worker)
            .fetch_optional(&mut **tx)
            .await?)
    }

    /// Mark a claimed job finished (`state = 'done'`), clearing the worker
    /// lock. `last_error` carries a non-fatal reason (e.g. a REJECT-path
    /// probe result) for admin visibility.
    pub async fn mark_done(
        pool: &sqlx::PgPool,
        id: i64,
        last_error: Option<&str>,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE jobs
            SET state = 'done', last_error = $2, locked_by = NULL, locked_at = NULL
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(last_error)
        .fetch_optional(pool)
        .await?)
    }

    /// Dead-letter a claimed job: permanent failure, admin-visible error.
    pub async fn dead_letter(
        pool: &sqlx::PgPool,
        id: i64,
        error: &str,
    ) -> Result<Option<Self>, DbError> {
        Ok(sqlx::query_as::<_, Self>(
            r#"
            UPDATE jobs
            SET state = 'dead', last_error = $2, locked_by = NULL, locked_at = NULL
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(error)
        .fetch_optional(pool)
        .await?)
    }
}

const CLAIM_NEXT_DUE_SQL: &str = r#"
    UPDATE jobs
    SET state = 'running', locked_by = $1, locked_at = now()
    WHERE id = (
        SELECT id FROM jobs
        WHERE state = 'queued' AND run_after <= now()
        ORDER BY run_after
        LIMIT 1
        FOR UPDATE SKIP LOCKED
    )
    RETURNING *
"#;
