//! Errors from the federation core. The inbox handler maps these to HTTP
//! statuses (see `crate::axum` in the server crate).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::egress::EgressError;

/// Aggregate error for federation operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// SQLx database error.
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    /// DB model error.
    #[error("database model error: {0}")]
    DbModel(#[from] toottok_db::error::DbError),
    /// ActivityPub federation crate error (signature verification, URL checks,
    /// fetch, parse).
    #[error("federation crate error: {0}")]
    Federation(#[from] activitypub_federation::error::Error),
    /// Outbound network/egress-guard error.
    #[error("egress error: {0}")]
    Egress(#[from] EgressError),
    /// Serialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// URL parse error.
    #[error("url parse error: {0}")]
    Url(#[from] url::ParseError),
    /// HTTP error.
    #[error("http error: {0}")]
    Http(#[from] axum::http::Error),
    /// reqwest error.
    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    /// reqwest-middleware error.
    #[error("reqwest middleware error: {0}")]
    ReqwestMiddleware(#[from] reqwest_middleware::Error),
    /// HTTP signature signing error.
    #[error("signing error: {0}")]
    Sign(#[from] http_signature_normalization_reqwest::SignError),
    /// RSA error.
    #[error("rsa error: {0}")]
    Rsa(#[from] rsa::errors::Error),
    /// Federation config build error.
    #[error("config error: {0}")]
    Config(String),
    /// Any other message-carrying failure.
    #[error("{0}")]
    Other(String),
}

impl From<activitypub_federation::config::FederationConfigBuilderError> for Error {
    fn from(e: activitypub_federation::config::FederationConfigBuilderError) -> Self {
        Error::Config(e.to_string())
    }
}

impl Error {
    /// Short string used for `jobs.last_error`.
    pub fn detail(&self) -> String {
        self.to_string()
    }
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Other(s)
    }
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Other(s.to_string())
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, title, body) = match &self {
            // HTTP signature invalid / missing -> 401 (RFC 7235).
            Error::Federation(activitypub_federation::error::Error::ActivitySignatureInvalid) => (
                StatusCode::UNAUTHORIZED,
                "signature invalid",
                self.to_string(),
            ),
            Error::Federation(activitypub_federation::error::Error::ActivityBodyDigestInvalid) => (
                StatusCode::UNAUTHORIZED,
                "body digest invalid",
                self.to_string(),
            ),
            Error::Federation(activitypub_federation::error::Error::ParseReceivedActivity {
                ..
            }) => (
                StatusCode::BAD_REQUEST,
                "unparseable activity",
                self.to_string(),
            ),
            Error::Federation(activitypub_federation::error::Error::UrlVerificationError(_))
            | Error::Federation(activitypub_federation::error::Error::DomainResolveError(_)) => (
                StatusCode::BAD_REQUEST,
                "url verification failed",
                self.to_string(),
            ),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal error",
                self.to_string(),
            ),
        };
        (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/problem+json")],
            serde_json::to_vec(&serde_json::json!({
                "title": title,
                "status": status.as_u16(),
                "detail": body,
            }))
            .unwrap_or_else(|_| b"{}".to_vec()),
        )
            .into_response()
    }
}
