use serde_json::Value;
use sqlx::PgPool;

use crate::error::DbError;

/// Write an audit_log entry for an admin action (ARCHITECTURE.md §8:
/// "audit_log for all admin actions"). Every mutating admin endpoint calls this.
pub async fn log(
    pool: &PgPool,
    admin_actor_id: i64,
    action: &str,
    target_type: &str,
    target_id: Option<i64>,
    payload: &Value,
) -> Result<(), DbError> {
    sqlx::query(
        r#"
        INSERT INTO audit_log (admin_actor_id, action, target_type, target_id, payload)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(admin_actor_id)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(payload)
    .execute(pool)
    .await?;
    Ok(())
}
