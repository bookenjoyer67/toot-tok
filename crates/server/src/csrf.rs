//! CSRF protection (ARCHITECTURE.md §8): SameSite=Lax cookies PLUS a custom
//! `X-Toottok-CSRF` header check on cookie-authenticated state-changing
//! requests. The header value is the per-session `csrf_token` the server
//! returned at login/`me`; a mismatch (or missing header) answers 403
//! problem+json. Login/register and read-only methods are exempt.

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use toottok_db::session::Session;

use crate::problem::problem;
use crate::session::session_token_from_headers;
use crate::AppState;

pub const CSRF_HEADER: &str = "x-toottok-csrf";

/// Routes that intentionally run without a session cookie (or that a client
/// reaches before any cookie exists); nothing cookie-authenticated is being
/// mutated, so the header check would be meaningless.
const CSRF_EXEMPT_PATHS: [&str; 2] = ["/api/v1/auth/login", "/api/v1/auth/register"];

pub async fn csrf_middleware(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let method = req.method();
    if matches!(
        method,
        &Method::GET | &Method::HEAD | &Method::OPTIONS | &Method::TRACE
    ) {
        return next.run(req).await;
    }
    let path = req.uri().path();
    if CSRF_EXEMPT_PATHS.contains(&path) {
        return next.run(req).await;
    }

    // No session cookie → nothing cookie-authenticated to protect; the
    // required-auth extractor (if the route needs one) answers 401 itself.
    let Some(token) = session_token_from_headers(req.headers()) else {
        return next.run(req).await;
    };

    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };

    let token_hash = toottok_db::password::hash_token(&token);
    let session = match Session::fetch_by_id(pool, &token_hash).await {
        Ok(Some(s)) => s,
        _ => return next.run(req).await,
    };

    let provided = req
        .headers()
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !constant_time_eq(provided, &session.csrf_token) {
        return problem(
            StatusCode::FORBIDDEN,
            "csrf token mismatch",
            "X-Toottok-CSRF header is required on cookie-authenticated state-changing requests",
        );
    }
    next.run(req).await
}

/// Length plus byte-for-byte comparison without short-circuiting on the first
/// differing byte (best-effort constant time; header lengths are public).
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a
        .bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y));
    diff == 0
}

/// Convenience for handlers that already hold an [`AuthUser`]: verify the
/// header directly. Not used by the current routes (the middleware covers
/// them), but kept for handlers mounted outside the CSRF layer.
#[allow(dead_code)]
pub fn verify(provided: Option<&str>, expected: &str) -> bool {
    constant_time_eq(provided.unwrap_or(""), expected)
}
