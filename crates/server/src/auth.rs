//! Account creation, session login/logout, and the D5 email-token flows
//! (email verification + password reset).
//!
//! Sessions: a random 32-byte url-safe token is handed to the client as the
//! `toottok_session` cookie; only its SHA-256 lives in `sessions.id`.
//! `register`/`login` are CSRF-exempt; every other state-changing,
//! cookie-authenticated route must echo the session `csrf_token` in the
//! `X-Toottok-CSRF` header (enforced by the CSRF middleware).

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use chrono::Utc;
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use toottok_db::actor::Actor;
use toottok_db::email_token::EmailToken;
use toottok_db::error::DbError;
use toottok_db::session::Session;
use toottok_db::settings::Setting;
use toottok_db::user::User;

use crate::keys::generate_actor_keypair;
use crate::mail::Mailer;
use crate::problem::problem;
use crate::session::{AuthUser, SESSION_COOKIE};
use crate::AppState;

/// Session lifetime: 30 days (cookie `Max-Age` matches the row `expires_at`).
const SESSION_LIFETIME_SECS: i64 = 30 * 24 * 60 * 60;
/// Email tokens (verify + password reset) expire after an hour.
const EMAIL_TOKEN_TTL_SECS: i64 = 60 * 60;

/// `registration_mode` values (settings key). Default `open` when no row.
const REGISTRATION_OPEN: &str = "open";
const REGISTRATION_APPROVAL: &str = "approval";
const REGISTRATION_INVITE: &str = "invite";

/// A 32-byte url-safe (base64url, no padding) opaque token.
fn random_urlsafe_token() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The `Set-Cookie` value for a fresh session. `Secure` only when the deploy
/// terminates TLS (`config.behind_tls`); off by default for LAN dev.
fn session_cookie(token: &str, behind_tls: bool) -> HeaderValue {
    let mut value = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_LIFETIME_SECS}"
    );
    if behind_tls {
        value.push_str("; Secure");
    }
    HeaderValue::from_str(&value).expect("session cookie header is valid")
}

/// Expire the cookie (logout / account deletion).
pub(crate) fn clear_cookie() -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
    ))
    .expect("clear-cookie header is valid")
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    username: String,
    email: Option<String>,
    password: String,
}

/// POST /api/v1/auth/register — create actor (real RSA-2048 keypair) + user,
/// honoring `registration_mode` (open → active, approval → pending, invite →
/// 403). Duplicate username/email answer 409 problem+json.
pub async fn register(
    State(state): State<AppState>,
    body: axum::Json<RegisterRequest>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };

    if let Err(resp) = validate_register(&body) {
        return resp;
    }

    let mode = match registration_mode(pool).await {
        Ok(m) => m,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    let status = match mode.as_str() {
        REGISTRATION_OPEN => "active",
        REGISTRATION_APPROVAL => "pending",
        REGISTRATION_INVITE => {
            return problem(
                StatusCode::FORBIDDEN,
                "registration closed",
                "this instance is invite-only",
            )
        }
        other => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid setting",
                format!("unknown registration_mode: {other}"),
            )
        }
    };

    let password_hash = match toottok_db::password::hash_password(&body.password) {
        Ok(h) => h,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "password hashing failed",
                format!("{e}"),
            )
        }
    };

    let (public_key_pem, private_key_pem) = match generate_actor_keypair() {
        Ok(kp) => kp,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "key generation failed",
                format!("{e}"),
            )
        }
    };

    let ap_id = format!("{}/users/{}", state.cfg.public_base_url(), body.username);
    let shared_inbox = format!("{}/ap/inbox", state.cfg.public_base_url());
    let actor = match Actor::create(
        pool,
        &body.username,
        None,
        "person",
        &public_key_pem,
        Some(&private_key_pem),
        &format!("{ap_id}/inbox"),
        Some(&shared_inbox),
        &format!("{ap_id}/outbox"),
        &format!("{ap_id}/followers"),
        &ap_id,
    )
    .await
    {
        Ok(a) => a,
        Err(e) if e.is_unique_violation() => {
            return problem(
                StatusCode::CONFLICT,
                "username taken",
                format!("username @{} is already taken", body.username),
            )
        }
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    let email = body
        .email
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty());
    let user = match User::create_with_status(pool, actor.id, email, &password_hash, status).await {
        Ok(u) => u,
        Err(e) if e.is_unique_violation() => {
            let _ = sqlx::query("DELETE FROM actors WHERE id = $1")
                .bind(actor.id)
                .execute(pool)
                .await;
            return problem(
                StatusCode::CONFLICT,
                "email taken",
                "an account with this email already exists",
            );
        }
        Err(e) => {
            let _ = sqlx::query("DELETE FROM actors WHERE id = $1")
                .bind(actor.id)
                .execute(pool)
                .await;
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            );
        }
    };

    // Open registration with an email: immediately issue a verify token (D5).
    if mode == REGISTRATION_OPEN && user.email.is_some() {
        if let Some(email) = &user.email {
            issue_email_token(pool, &*state.mailer, user.id, email, "verify").await;
        }
    }

    (
        StatusCode::CREATED,
        Json(json!({
            "actor_id": actor.id,
            "username": body.username,
            "status": status,
        })),
    )
        .into_response()
}

/// Email gate: `^[^@\s]+@[^@\s]+\.[a-zA-Z]{2,}$` — a non-empty local part, a
/// `@`, a domain with at least one dot, and an ASCII-alpha TLD of ≥2 letters.
/// No whitespace, no extra `@`. Implemented by hand so the regex crate stays
/// out of the tree.
fn is_valid_email(email: &str) -> bool {
    if email.chars().any(char::is_whitespace) {
        return false;
    }
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    if parts.next().is_some() {
        return false; // more than one '@'
    }
    if local.is_empty() {
        return false;
    }
    let Some((base, tld)) = domain.rsplit_once('.') else {
        return false; // needs a dotted TLD
    };
    if base.is_empty() {
        return false;
    }
    tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
}

/// Username rules map to `^[a-z0-9_]{3,30}$` (lowercase only — the partial
/// unique index is case-insensitive, so we reject mixed case at the gate);
/// passwords must be ≥10 chars; an email, when given, must look like one.
#[allow(clippy::result_large_err)]
fn validate_register(body: &RegisterRequest) -> Result<(), Response> {
    let username = body.username.trim();
    let valid = (3..=30).contains(&username.len())
        && username
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !valid {
        return Err(problem(
            StatusCode::BAD_REQUEST,
            "invalid username",
            "username must be 3–30 chars of lowercase a-z, 0-9, or _",
        ));
    }
    if body.password.len() < 10 {
        return Err(problem(
            StatusCode::BAD_REQUEST,
            "weak password",
            "password must be at least 10 characters",
        ));
    }
    if let Some(email) = &body.email {
        let email = email.trim();
        if !email.is_empty() && !is_valid_email(email) {
            return Err(problem(
                StatusCode::BAD_REQUEST,
                "invalid email",
                "email address is not valid",
            ));
        }
    }
    Ok(())
}

/// Read `registration_mode` from settings; default `open` when no row exists.
async fn registration_mode(pool: &PgPool) -> Result<String, DbError> {
    match Setting::fetch_by_key(pool, "registration_mode").await? {
        Some(s) => Ok(s.value.as_str().unwrap_or(REGISTRATION_OPEN).to_string()),
        None => Ok(REGISTRATION_OPEN.to_string()),
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    username_or_email: String,
    password: String,
}

/// POST /api/v1/auth/login — verify argon2id, create a session, set the cookie,
/// return `{actor_id, username, status, csrf_token}`. Pending-approval and
/// deleted accounts are refused.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::Json<LoginRequest>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };

    let user = match find_user_for_login(pool, &body.username_or_email).await {
        Ok(Some(u)) => u,
        Ok(None) => return invalid_credentials(),
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    if user.deleted_at.is_some() {
        return invalid_credentials();
    }
    if user.status != "active" {
        return problem(
            StatusCode::FORBIDDEN,
            "account pending approval",
            "this account is still awaiting admin approval",
        );
    }
    let Some(stored_hash) = user.password_hash.as_deref() else {
        return invalid_credentials();
    };
    if !toottok_db::password::verify_password(&body.password, stored_hash) {
        return invalid_credentials();
    }

    let actor = match Actor::fetch_by_id(pool, user.actor_id).await {
        Ok(Some(a)) => a,
        Ok(None) => return invalid_credentials(),
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    if actor.deleted_at.is_some() {
        return invalid_credentials();
    }
    if actor.suspended_at.is_some() {
        return problem(
            StatusCode::FORBIDDEN,
            "account suspended",
            "this account has been suspended",
        );
    }

    let token = random_urlsafe_token();
    let token_hash = toottok_db::password::hash_token(&token);
    let csrf_token = random_urlsafe_token();
    let expires_at = Utc::now() + chrono::Duration::seconds(SESSION_LIFETIME_SECS);
    let ip = forwarded_ip(&headers);
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    if let Err(e) = Session::create(
        pool,
        &token_hash,
        user.id,
        expires_at,
        ip.as_deref(),
        ua.as_deref(),
        &csrf_token,
    )
    .await
    {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }

    let set_cookie = session_cookie(&token, state.cfg.behind_tls);
    (
        StatusCode::OK,
        [
            (header::SET_COOKIE, set_cookie),
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
        ],
        serde_json::to_vec(&json!({
            "actor_id": actor.id,
            "username": actor.username,
            "status": user.status,
            "csrf_token": csrf_token,
        }))
        .expect("login response serializes"),
    )
        .into_response()
}

/// Resolve `username_or_email`: a local username first, then a users.email row.
async fn find_user_for_login(pool: &PgPool, login: &str) -> Result<Option<User>, DbError> {
    if let Some(actor) = Actor::fetch_by_username_local(pool, login).await? {
        return User::fetch_by_actor_id(pool, actor.id).await;
    }
    User::fetch_by_email(pool, login).await
}

/// First hop of `X-Forwarded-For` is the immediate peer behind a proxy; record
/// it when present (the value is only for the session row, never trusted for
/// enforcement).
fn forwarded_ip(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next().map(str::trim))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn invalid_credentials() -> Response {
    problem(
        StatusCode::UNAUTHORIZED,
        "invalid credentials",
        "invalid username or password",
    )
}

/// POST /api/v1/auth/logout — delete the session row and clear the cookie.
pub async fn logout(State(state): State<AppState>, auth: AuthUser) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if let Err(e) = Session::delete_by_id(pool, &auth.session.id).await {
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

#[derive(Debug, Deserialize)]
pub struct RequestResetRequest {
    email: String,
}

/// Fixed plaintext burned through argon2id when a reset is requested for an
/// unknown/ineligible email, so response timing does not reveal account
/// existence.
const DUMMY_RESET_PASSWORD: &str = "toottok-dummy-password-reset";

/// Lazy argon2id hash of [`DUMMY_RESET_PASSWORD`]; computed once, then only
/// verified against (never used to authenticate anything).
fn dummy_password_hash() -> &'static str {
    static HASH: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HASH.get_or_init(|| {
        toottok_db::password::hash_password(DUMMY_RESET_PASSWORD)
            .expect("dummy argon2 hash always hashes")
    })
}

/// POST /api/v1/auth/request-reset — always 202 (no user enumeration); when the
/// email matches an active account a 1-hour reset token is mailed (logged).
/// The unknown-email path burns the same argon2 cost as a real lookup/verify.
pub async fn request_reset(
    State(state): State<AppState>,
    body: axum::Json<RequestResetRequest>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let email = body.email.trim();
    if !is_valid_email(email) {
        return problem(
            StatusCode::BAD_REQUEST,
            "invalid email",
            "email address is not valid",
        );
    }
    match User::fetch_by_email(pool, email).await {
        Ok(Some(user)) if user.deleted_at.is_none() && user.status == "active" => {
            if let Some(email) = &user.email {
                issue_email_token(pool, &*state.mailer, user.id, email, "password_reset").await;
            }
        }
        // Unknown or ineligible account: burn the same argon2 cost as the
        // known-account path so timing cannot enumerate emails.
        _ => {
            let _ =
                toottok_db::password::verify_password(DUMMY_RESET_PASSWORD, dummy_password_hash());
        }
    }
    (StatusCode::ACCEPTED, Json(json!({}))).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ResetRequest {
    token: String,
    new_password: String,
}

/// POST /api/v1/auth/reset — consume the reset token, set the new password,
/// revoke every session and outstanding reset link.
pub async fn reset(State(state): State<AppState>, body: axum::Json<ResetRequest>) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if body.new_password.len() < 10 {
        return problem(
            StatusCode::BAD_REQUEST,
            "weak password",
            "password must be at least 10 characters",
        );
    }

    let token_hash = toottok_db::password::hash_token(&body.token);
    let user = match EmailToken::consume(pool, &token_hash, "password_reset").await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return problem(
                StatusCode::BAD_REQUEST,
                "invalid token",
                "reset token is invalid, expired, or already used",
            )
        }
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };

    let new_hash = match toottok_db::password::hash_password(&body.new_password) {
        Ok(h) => h,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "password hashing failed",
                format!("{e}"),
            )
        }
    };
    if let Err(e) = User::set_password(pool, user.id, &new_hash).await {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }
    if let Err(e) = EmailToken::invalidate_for_user(pool, user.id, "password_reset").await {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }
    if let Err(e) = Session::delete_for_user(pool, user.id).await {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
pub struct VerifyEmailRequest {
    token: String,
}

/// POST /api/v1/auth/verify-email — consume the verify token and stamp
/// `users.email_verified_at`.
pub async fn verify_email(
    State(state): State<AppState>,
    body: axum::Json<VerifyEmailRequest>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let token_hash = toottok_db::password::hash_token(&body.token);
    let user = match EmailToken::consume(pool, &token_hash, "verify").await {
        Ok(Some(u)) => u,
        Ok(None) => {
            return problem(
                StatusCode::BAD_REQUEST,
                "invalid token",
                "verification token is invalid, expired, or already used",
            )
        }
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    if let Err(e) = User::set_email_verified(pool, user.id).await {
        return problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        );
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Create an `email_tokens` row (kind `verify` or `password_reset`) and mail
/// the plaintext token. Best-effort: a token-store failure only logs, never
/// fails the surrounding request.
async fn issue_email_token(
    pool: &PgPool,
    mailer: &dyn Mailer,
    user_id: i64,
    email: &str,
    kind: &str,
) {
    let token = random_urlsafe_token();
    let token_hash = toottok_db::password::hash_token(&token);
    let expires_at = Utc::now() + chrono::Duration::seconds(EMAIL_TOKEN_TTL_SECS);
    if let Err(e) = EmailToken::create(pool, user_id, kind, &token_hash, expires_at).await {
        tracing::warn!(user_id, kind, error = %e, "failed to persist email token");
        return;
    }
    let subject = match kind {
        "verify" => "TootTok email verification",
        _ => "TootTok password reset",
    };
    let body = format!("TootTok {kind} code: {token}\nThis code expires in 1 hour.\n");
    mailer.send(email, subject, &body);
}
