//! RFC 9457 `application/problem+json` error responses.

use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};

/// Build a problem+json response. `type` stays `about:blank` for now
/// (no custom URI scheme); clients key off `status` + `title`.
pub fn problem(status: StatusCode, title: &'static str, detail: impl Into<String>) -> Response {
    let body = serde_json::to_vec(&serde_json::json!({
        "type": "about:blank",
        "title": title,
        "status": status.as_u16(),
        "detail": detail.into(),
    }))
    .expect("problem+json serialization cannot fail");
    (
        status,
        [(header::CONTENT_TYPE, "application/problem+json")],
        body,
    )
        .into_response()
}
