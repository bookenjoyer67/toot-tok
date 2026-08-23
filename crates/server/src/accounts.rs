//! `/api/v1/accounts/me` — current account profile (GET/PATCH), avatar upload,
//! and local-half account deletion.

use axum::extract::{Multipart, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;
use toottok_db::actor::Actor;
use toottok_db::email_token::EmailToken;
use toottok_db::session::Session;
use toottok_db::user::User;

use crate::auth::clear_cookie;
use crate::problem::problem;
use crate::session::AuthUser;
use crate::AppState;

/// Avatar cap: 2 MiB of image bytes.
const AVATAR_SIZE_CAP: usize = 2 * 1024 * 1024;

fn profile(actor: &Actor, user: &User, csrf_token: &str) -> serde_json::Value {
    json!({
        "actor_id": actor.id,
        "username": actor.username,
        "display_name": actor.display_name,
        "summary": actor.summary,
        "avatar_path": actor.avatar_path,
        "email": user.email,
        "email_verified": user.email_verified_at.is_some(),
        "is_admin": user.is_admin,
        "status": user.status,
        "created_at": actor.created_at,
        "csrf_token": csrf_token,
    })
}

/// GET /api/v1/accounts/me — current actor+user profile (401 when unauthenticated).
pub async fn me(State(state): State<AppState>, auth: AuthUser) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let actor = match Actor::fetch_by_id(pool, auth.actor.id).await {
        Ok(Some(a)) => a,
        Ok(None) => return problem(StatusCode::NOT_FOUND, "account not found", "actor vanished"),
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        serde_json::to_vec(&profile(&actor, &auth.user, &auth.session.csrf_token))
            .expect("profile serializes"),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct PatchMeRequest {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    summary: Option<String>,
}

/// PATCH /api/v1/accounts/me — update display_name / summary on the actor.
pub async fn patch_me(
    State(state): State<AppState>,
    auth: AuthUser,
    body: Json<PatchMeRequest>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let actor = match Actor::update_profile(
        pool,
        auth.actor.id,
        body.display_name.as_deref(),
        body.summary.as_deref(),
    )
    .await
    {
        Ok(Some(a)) => a,
        Ok(None) => return problem(StatusCode::NOT_FOUND, "account not found", "actor vanished"),
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    (
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        serde_json::to_vec(&profile(&actor, &auth.user, &auth.session.csrf_token))
            .expect("profile serializes"),
    )
        .into_response()
}

/// POST /api/v1/accounts/me/avatar — multipart image (png/jpeg by magic bytes,
/// ≤2 MiB), stored under `avatars/{actor_id}.{ext}`, stamped on the actor.
pub async fn avatar(
    State(state): State<AppState>,
    auth: AuthUser,
    mut multipart: Multipart,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };

    let data = match crate::upload::read_file_field(&mut multipart, AVATAR_SIZE_CAP).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return problem(
                StatusCode::BAD_REQUEST,
                "missing file",
                "multipart field 'file' is required",
            )
        }
        Err(resp) => return resp,
    };
    let Some((ext, _mime)) = sniff_image(&data) else {
        return problem(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported image",
            "avatar must be a PNG or JPEG image",
        );
    };

    let key = format!("avatars/{}.{}", auth.actor.id, ext);
    if let Err(e) = state.store.save_bytes(&key, &data).await {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage error",
            format!("{e}"),
        );
    }
    match Actor::set_avatar_path(pool, auth.actor.id, &key).await {
        Ok(Some(_)) => {}
        Ok(None) => return problem(StatusCode::NOT_FOUND, "account not found", "actor vanished"),
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    }

    (StatusCode::OK, Json(json!({ "avatar_path": key }))).into_response()
}

/// PNG: `89 50 4E 47 0D 0A 1A 0A`; JPEG: `FF D8 FF`. Returns `(ext, mime)`.
fn sniff_image(data: &[u8]) -> Option<(&'static str, &'static str)> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(("png", "image/png"))
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(("jpg", "image/jpeg"))
    } else {
        None
    }
}

#[derive(Debug, Deserialize)]
pub struct DeleteMeRequest {
    password: String,
}

/// DELETE /api/v1/accounts/me — local-half account deletion: password confirm,
/// tombstone the actor, erase personal/credential columns, flip the account's
/// clips to `deleted`, and revoke sessions/tokens.
pub async fn delete_me(
    State(state): State<AppState>,
    auth: AuthUser,
    body: Json<DeleteMeRequest>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };

    let Some(stored_hash) = auth.user.password_hash.as_deref() else {
        return problem(
            StatusCode::UNAUTHORIZED,
            "invalid password",
            "no password on record",
        );
    };
    if !toottok_db::password::verify_password(&body.password, stored_hash) {
        return problem(
            StatusCode::UNAUTHORIZED,
            "invalid password",
            "current password is required",
        );
    }

    // Flip every live clip to deleted first (status 'deleted' + tombstone time).
    if let Err(e) = sqlx::query(
        r#"
        UPDATE clips
        SET deleted_at = now(), status = 'deleted'
        WHERE actor_id = $1 AND deleted_at IS NULL
        "#,
    )
    .bind(auth.actor.id)
    .execute(pool)
    .await
    {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }

    // Best-effort media file deletion; the media GC sweeps whatever is left.
    let asset_paths: Vec<String> = match sqlx::query_scalar::<_, String>(
        r#"
        SELECT ma.path
        FROM media_assets ma
        JOIN clips c ON c.id = ma.clip_id
        WHERE c.actor_id = $1
        "#,
    )
    .bind(auth.actor.id)
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    for key in &asset_paths {
        let _ = state.store.delete(key).await;
    }

    // Tombstone the actor object so late federation sees a deletion.
    if let Err(e) = sqlx::query(
        r#"
        INSERT INTO tombstones (ap_id, type)
        VALUES ($1, 'Person')
        ON CONFLICT (ap_id) DO NOTHING
        "#,
    )
    .bind(&auth.actor.ap_id)
    .execute(pool)
    .await
    {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }

    // Erasure: NULL out email/hash/totp, stamp deleted_at.
    if let Err(e) = User::mark_deleted_and_erase(pool, auth.user.id).await {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }
    if let Err(e) = Actor::mark_deleted(pool, auth.actor.id).await {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }

    // Revoke all sessions and email tokens.
    if let Err(e) = Session::delete_for_user(pool, auth.user.id).await {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }
    if let Err(e) = EmailToken::delete_for_user(pool, auth.user.id).await {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }

    (
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, clear_cookie())],
    )
        .into_response()
}
