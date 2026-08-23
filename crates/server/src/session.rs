//! Session cookie parsing + the `AuthUser` request extractor.
//!
//! `AuthUser` implements `FromRequestParts<AppState>`: a required extractor
//! that answers `401 problem+json` when the cookie is missing, unknown,
//! expired, or belongs to a deleted/suspended-ineligible account. Handlers that
//! want to work with or without a login use [`OptionalAuthUser`].

use std::convert::Infallible;

use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::Response;
use chrono::Utc;
use toottok_db::actor::Actor;
use toottok_db::error::DbError;
use toottok_db::session::Session;
use toottok_db::user::User;

use crate::problem::problem;
use crate::AppState;

pub const SESSION_COOKIE: &str = "toottok_session";

/// An authenticated local account: the user, its actor, and the live session.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub user: User,
    pub actor: Actor,
    pub session: Session,
}

/// Pull the raw `toottok_session` cookie value (the plaintext token) out of a
/// request. The DB only ever stores its SHA-256.
pub fn session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in cookie.split(';') {
        let mut parts = pair.trim().splitn(2, '=');
        if parts.next()? == SESSION_COOKIE {
            return parts.next().map(|v| v.trim().to_string());
        }
    }
    None
}

fn unauthorized() -> Response {
    problem(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "a valid toottok_session cookie is required",
    )
}

fn db_error(e: DbError) -> Response {
    problem(
        StatusCode::INTERNAL_SERVER_ERROR,
        "database error",
        format!("{e}"),
    )
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let Some(token) = session_token_from_headers(&parts.headers) else {
            return Err(unauthorized());
        };
        let Some(pool) = &state.pool else {
            return Err(problem(
                StatusCode::SERVICE_UNAVAILABLE,
                "database unavailable",
                "database is not configured",
            ));
        };

        let token_hash = toottok_db::password::hash_token(&token);
        let Some(session) = Session::fetch_by_id(pool, &token_hash)
            .await
            .map_err(db_error)?
        else {
            return Err(unauthorized());
        };
        if session.expires_at <= Utc::now() {
            let _ = Session::delete_by_id(pool, &session.id).await;
            return Err(unauthorized());
        }
        let Some(user) = User::fetch_by_id(pool, session.user_id)
            .await
            .map_err(db_error)?
        else {
            return Err(unauthorized());
        };
        if user.deleted_at.is_some() || user.status != "active" {
            return Err(unauthorized());
        }
        let Some(actor) = Actor::fetch_by_id(pool, user.actor_id)
            .await
            .map_err(db_error)?
        else {
            return Err(unauthorized());
        };
        if actor.deleted_at.is_some() || actor.suspended_at.is_some() {
            return Err(unauthorized());
        }
        Ok(AuthUser {
            user,
            actor,
            session,
        })
    }
}

/// Optional variant: `Ok(None)` when the request has no usable session. Lets a
/// handler degrade gracefully instead of hard-failing on the auth extractor.
pub struct OptionalAuthUser(pub Option<AuthUser>);

impl FromRequestParts<AppState> for OptionalAuthUser {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match AuthUser::from_request_parts(parts, state).await {
            Ok(auth) => Ok(Self(Some(auth))),
            Err(_) => Ok(Self(None)),
        }
    }
}
