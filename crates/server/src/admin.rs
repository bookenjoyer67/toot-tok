//! Admin API — every action guarded by `is_admin` + CSRF, and every mutating
//! action writes an `audit_log` entry.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use toottok_db::actor::Actor;
use toottok_db::audit;
use toottok_db::session::Session;
use toottok_db::settings::Setting;
use toottok_db::user::User;

use crate::problem::problem;
use crate::session::AuthUser;
use crate::AppState;

/// Keys admin may read/write through the settings API. Numeric keys validate
/// as numbers; the two mode keys validate against their enums.
const SETTINGS_WHITELIST: [&str; 7] = [
    "registration_mode",
    "federation_mode",
    "upload_size_cap_mb",
    "clip_max_seconds",
    "per_user_storage_quota_mb",
    "ffmpeg_threads",
    "jobs_job_timeout_secs",
];

fn default_setting_value(key: &str) -> Value {
    match key {
        "registration_mode" | "federation_mode" => json!("open"),
        "upload_size_cap_mb" => json!(500),
        "clip_max_seconds" => json!(180),
        "per_user_storage_quota_mb" => json!(0),
        "ffmpeg_threads" => json!(2),
        "jobs_job_timeout_secs" => json!(900),
        _ => json!(null),
    }
}

#[allow(clippy::result_large_err)]
fn require_admin(auth: &AuthUser) -> Result<(), Response> {
    if auth.user.is_admin {
        Ok(())
    } else {
        Err(problem(
            StatusCode::FORBIDDEN,
            "forbidden",
            "admin privileges are required",
        ))
    }
}

/// Write an audit entry without ever failing the request. A mutation must not
/// silently persist unlogged; an audit-log outage is surfaced as a loud
/// `tracing::error` instead of a misleading 500 after the fact.
async fn audit_logged(
    pool: &PgPool,
    auth: &AuthUser,
    action: &str,
    target_type: &str,
    target_id: Option<i64>,
    payload: &Value,
) {
    if let Err(e) = audit::log(pool, auth.actor.id, action, target_type, target_id, payload).await {
        tracing::error!(error = %e, action, "admin audit_log write failed");
    }
}

/// Best-effort record of an attempted-but-failed admin action (unknown target,
/// DB error, …). Never fatal; failures of the failure-log itself are logged.
async fn audit_attempted(
    pool: &PgPool,
    auth: &AuthUser,
    action: &str,
    target_type: &str,
    target_id: Option<i64>,
    payload: &Value,
) {
    audit_logged(
        pool,
        auth,
        &format!("{action}.attempted"),
        target_type,
        target_id,
        payload,
    )
    .await;
}

#[derive(Debug, Deserialize)]
pub struct ListUsersParams {
    state: Option<String>,
}

#[derive(serde::Serialize, sqlx::FromRow)]
struct AdminUserRow {
    user_id: i64,
    actor_id: i64,
    username: String,
    email: Option<String>,
    status: String,
    is_admin: bool,
    suspended_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
}

/// GET /api/v1/admin/users?state=pending|all — non-deleted accounts, optionally
/// filtered to pending-approval signups.
pub async fn list_users(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(params): Query<ListUsersParams>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if let Err(resp) = require_admin(&auth) {
        return resp;
    }

    let filter = match params.state.as_deref() {
        Some("pending") => "AND u.status = 'pending'",
        _ => "",
    };
    let query = format!(
        r#"
        SELECT u.id AS user_id, u.actor_id, a.username, u.email, u.status,
               u.is_admin, a.suspended_at, u.created_at
        FROM users u
        JOIN actors a ON a.id = u.actor_id
        WHERE u.deleted_at IS NULL {filter}
        ORDER BY u.id
        "#
    );
    let rows = match sqlx::query_as::<_, AdminUserRow>(&query)
        .fetch_all(pool)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            audit_attempted(
                pool,
                &auth,
                "user.list",
                "user",
                None,
                &json!({ "state": params.state.unwrap_or_else(|| "all".to_string()) }),
            )
            .await;
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    };
    // Every admin action is audit-logged, reads included. Audit failures are
    // logged, never fatal.
    audit_logged(
        pool,
        &auth,
        "user.list",
        "user",
        None,
        &json!({ "state": params.state.unwrap_or_else(|| "all".to_string()) }),
    )
    .await;
    (
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )],
        serde_json::to_vec(&json!({ "users": rows })).expect("admin user list serializes"),
    )
        .into_response()
}

/// POST /api/v1/admin/users/{id}/approve — move an approval-mode signup to active.
pub async fn approve_user(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if let Err(resp) = require_admin(&auth) {
        return resp;
    }
    match User::set_status(pool, id, "active").await {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit_attempted(
                pool,
                &auth,
                "user.approve",
                "user",
                Some(id),
                &json!({ "user_id": id }),
            )
            .await;
            return problem(
                StatusCode::NOT_FOUND,
                "user not found",
                format!("no user {id}"),
            );
        }
        Err(e) => {
            audit_attempted(
                pool,
                &auth,
                "user.approve",
                "user",
                Some(id),
                &json!({ "user_id": id }),
            )
            .await;
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    }
    audit_logged(
        pool,
        &auth,
        "user.approve",
        "user",
        Some(id),
        &json!({ "user_id": id }),
    )
    .await;
    (
        StatusCode::OK,
        Json(json!({ "user_id": id, "status": "active" })),
    )
        .into_response()
}

/// POST /api/v1/admin/users/{id}/suspend — set `actors.suspended_at`.
pub async fn suspend_user(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if let Err(resp) = require_admin(&auth) {
        return resp;
    }
    let user = match User::fetch_by_id(pool, id).await {
        Ok(Some(u)) => u,
        Ok(None) => {
            audit_attempted(
                pool,
                &auth,
                "user.suspend",
                "user",
                Some(id),
                &json!({ "user_id": id }),
            )
            .await;
            return problem(
                StatusCode::NOT_FOUND,
                "user not found",
                format!("no user {id}"),
            );
        }
        Err(e) => {
            audit_attempted(
                pool,
                &auth,
                "user.suspend",
                "user",
                Some(id),
                &json!({ "user_id": id }),
            )
            .await;
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    };
    match Actor::suspend(pool, user.actor_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            audit_attempted(
                pool,
                &auth,
                "user.suspend",
                "user",
                Some(id),
                &json!({ "user_id": id, "actor_id": user.actor_id }),
            )
            .await;
            return problem(
                StatusCode::NOT_FOUND,
                "actor not found",
                "user has no actor",
            );
        }
        Err(e) => {
            audit_attempted(
                pool,
                &auth,
                "user.suspend",
                "user",
                Some(id),
                &json!({ "user_id": id, "actor_id": user.actor_id }),
            )
            .await;
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    }
    // A suspended account must lose every live session immediately; otherwise
    // the existing cookie keeps authenticating until it expires.
    if let Err(e) = Session::delete_for_user(pool, user.id).await {
        audit_logged(
            pool,
            &auth,
            "user.suspend",
            "user",
            Some(id),
            &json!({ "user_id": id, "actor_id": user.actor_id, "note": "session revocation failed" }),
        )
        .await;
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }
    audit_logged(
        pool,
        &auth,
        "user.suspend",
        "user",
        Some(id),
        &json!({ "user_id": id, "actor_id": user.actor_id }),
    )
    .await;
    (
        StatusCode::OK,
        Json(json!({ "user_id": id, "suspended_at": chrono::Utc::now() })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct DomainBlockRequest {
    domain: String,
    public_note: Option<String>,
}

/// POST /api/v1/admin/domain-blocks — insert or update a domain block.
pub async fn create_domain_block(
    State(state): State<AppState>,
    auth: AuthUser,
    body: Json<DomainBlockRequest>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if let Err(resp) = require_admin(&auth) {
        return resp;
    }
    let domain = body.domain.trim().to_lowercase();
    if domain.is_empty() || domain.contains('/') {
        return problem(
            StatusCode::BAD_REQUEST,
            "invalid domain",
            "domain must be a bare hostname",
        );
    }
    if let Err(e) = sqlx::query(
        r#"
        INSERT INTO domain_blocks (domain, public_note)
        VALUES ($1, $2)
        ON CONFLICT (domain) DO UPDATE SET public_note = EXCLUDED.public_note
        "#,
    )
    .bind(&domain)
    .bind(&body.public_note)
    .execute(pool)
    .await
    {
        audit_attempted(
            pool,
            &auth,
            "domain_block.create",
            "domain_block",
            None,
            &json!({ "domain": domain }),
        )
        .await;
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }
    audit_logged(
        pool,
        &auth,
        "domain_block.create",
        "domain_block",
        None,
        &json!({ "domain": domain }),
    )
    .await;
    (StatusCode::OK, Json(json!({ "domain": domain }))).into_response()
}

/// DELETE /api/v1/admin/domain-blocks/{domain} — remove a block.
pub async fn delete_domain_block(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(domain): Path<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if let Err(resp) = require_admin(&auth) {
        return resp;
    }
    if let Err(e) = sqlx::query("DELETE FROM domain_blocks WHERE domain = $1")
        .bind(&domain)
        .execute(pool)
        .await
    {
        audit_attempted(
            pool,
            &auth,
            "domain_block.delete",
            "domain_block",
            None,
            &json!({ "domain": domain }),
        )
        .await;
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }
    audit_logged(
        pool,
        &auth,
        "domain_block.delete",
        "domain_block",
        None,
        &json!({ "domain": domain }),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

/// GET /api/v1/admin/settings — the whitelisted keys with defaults for absent rows.
pub async fn get_settings(State(state): State<AppState>, auth: AuthUser) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if let Err(resp) = require_admin(&auth) {
        return resp;
    }
    let mut settings = serde_json::Map::new();
    for key in SETTINGS_WHITELIST {
        match Setting::fetch_by_key(pool, key).await {
            Ok(Some(s)) => {
                settings.insert(key.to_string(), s.value);
            }
            Ok(None) => {
                settings.insert(key.to_string(), default_setting_value(key));
            }
            Err(e) => {
                audit_attempted(
                    pool,
                    &auth,
                    "settings.read",
                    "setting",
                    None,
                    &json!({ "keys": SETTINGS_WHITELIST }),
                )
                .await;
                return problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database error",
                    format!("{e}"),
                );
            }
        }
    }
    audit_logged(
        pool,
        &auth,
        "settings.read",
        "setting",
        None,
        &json!({ "keys": SETTINGS_WHITELIST }),
    )
    .await;
    (
        [(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )],
        serde_json::to_vec(&json!({ "settings": settings })).expect("settings serializes"),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct PutSettingsRequest {
    settings: std::collections::HashMap<String, Value>,
}

/// PUT /api/v1/admin/settings — validate against the whitelist, persist, and
/// audit-log every changed key.
pub async fn put_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    body: Json<PutSettingsRequest>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if let Err(resp) = require_admin(&auth) {
        return resp;
    }
    for (key, value) in &body.settings {
        if let Err(resp) = validate_setting(key, value) {
            return resp;
        }
    }
    for (key, value) in &body.settings {
        if let Err(e) = Setting::set(pool, key, value).await {
            audit_attempted(
                pool,
                &auth,
                "settings.update",
                "setting",
                None,
                &json!({ "key": key, "value": value }),
            )
            .await;
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
        audit_logged(
            pool,
            &auth,
            "settings.update",
            "setting",
            None,
            &json!({ "key": key, "value": value }),
        )
        .await;
    }
    get_settings(State(state), auth).await
}

#[allow(clippy::result_large_err)]
fn validate_setting(key: &str, value: &Value) -> Result<(), Response> {
    if !SETTINGS_WHITELIST.contains(&key) {
        return Err(problem(
            StatusCode::BAD_REQUEST,
            "unknown setting",
            format!("{key} is not an admin-settable key"),
        ));
    }
    match key {
        "registration_mode" => {
            let ok = value
                .as_str()
                .is_some_and(|v| matches!(v, "open" | "approval" | "invite"));
            if !ok {
                return Err(problem(
                    StatusCode::BAD_REQUEST,
                    "invalid value",
                    "registration_mode must be open, approval, or invite",
                ));
            }
        }
        "federation_mode" => {
            let ok = value
                .as_str()
                .is_some_and(|v| matches!(v, "open" | "allowlist"));
            if !ok {
                return Err(problem(
                    StatusCode::BAD_REQUEST,
                    "invalid value",
                    "federation_mode must be open or allowlist",
                ));
            }
        }
        _ => {
            if !value.is_number() {
                return Err(problem(
                    StatusCode::BAD_REQUEST,
                    "invalid value",
                    format!("{key} must be a number"),
                ));
            }
        }
    }
    Ok(())
}
