//! GET/HEAD /assets/{clip_id}/{filename} — media serving with HTTP Range
//! support.
//!
//! Files are resolved through the `media_assets` table (kind/rendition row for
//! a clip whose storage key ends in `filename`), then streamed from
//! `media_dir` with `Content-Type` + `Accept-Ranges: bytes` + an immutable
//! `Cache-Control`. Single-range requests return `206 Partial Content` with
//! `Content-Range`; a suffix longer than the file serves the whole file as
//! `200` (RFC 7233 §2.1); malformed or unsatisfiable ranges return `416`;
//! unknown clips/assets return `404` problem+json. Responses are streamed
//! (seek + `tokio_util::io::ReaderStream`), never fully buffered.

use std::io::SeekFrom;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use toottok_db::media_asset::MediaAsset;

use crate::problem::problem;
use crate::AppState;

/// Asset responses never change once written (h264 rungs, posters, originals),
/// so they are safe to cache for a year and never revalidate.
pub const CACHE_CONTROL_IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// A clamped, satisfiable single byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

/// The outcome of parsing a `Range` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// Serve `206 Partial Content` for `[start, end]`.
    Partial(ByteRange),
    /// The requested suffix length is ≥ the file size (RFC 7233 §2.1: "the
    /// entire representation is used"): serve the whole file as `200 OK`.
    Full,
}

/// All parse failures (malformed, multi-range, unsatisfiable) answer `416`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeError;

/// Parse a single `Range:` header value against `size`. The `bytes=` unit
/// prefix is matched case-insensitively.
pub fn parse_byte_range(header: &str, size: u64) -> Result<RangeSpec, RangeError> {
    if size == 0 {
        return Err(RangeError);
    }
    let header = header.trim();
    if header.len() < 6 || !header[..6].eq_ignore_ascii_case("bytes=") {
        return Err(RangeError);
    }
    let rest = &header[6..];
    if rest.contains(',') {
        return Err(RangeError);
    }
    let Some((start_raw, end_raw)) = rest.split_once('-') else {
        return Err(RangeError);
    };
    let start = if start_raw.is_empty() {
        None
    } else {
        Some(start_raw.trim().parse::<u64>().map_err(|_| RangeError)?)
    };
    let end = if end_raw.is_empty() {
        None
    } else {
        Some(end_raw.trim().parse::<u64>().map_err(|_| RangeError)?)
    };

    match (start, end) {
        (Some(s), Some(e)) if s > e || s >= size => Err(RangeError),
        (Some(s), None) if s >= size => Err(RangeError),
        (None, Some(0)) => Err(RangeError),
        (Some(s), Some(e)) => Ok(RangeSpec::Partial(ByteRange {
            start: s,
            end: e.min(size - 1),
        })),
        (Some(s), None) => Ok(RangeSpec::Partial(ByteRange {
            start: s,
            end: size - 1,
        })),
        // Suffix length ≥ file size ⇒ the whole representation, 200 OK.
        (None, Some(suffix)) if suffix >= size => Ok(RangeSpec::Full),
        (None, Some(suffix)) => Ok(RangeSpec::Partial(ByteRange {
            start: size - suffix,
            end: size - 1,
        })),
        (None, None) => Err(RangeError),
    }
}

pub async fn asset(
    State(state): State<AppState>,
    Path((clip_id, filename)): Path<(i64, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };

    let asset = match MediaAsset::find_for_clip_filename(pool, clip_id, &filename).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            return problem(
                StatusCode::NOT_FOUND,
                "asset not found",
                format!("no asset named {filename} for clip {clip_id}"),
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

    let stored = match state.store.open(&asset.path).await {
        Ok(f) => f,
        Err(_) => {
            return problem(
                StatusCode::NOT_FOUND,
                "asset not found",
                format!("stored file for {filename} is missing"),
            )
        }
    };

    let size = stored.size_bytes;
    let mime = if asset.mime.is_empty() {
        mime_for_extension(&filename)
    } else {
        asset.mime.clone()
    };

    let file = match tokio::fs::File::open(&stored.path).await {
        Ok(f) => f,
        Err(_) => {
            return problem(
                StatusCode::NOT_FOUND,
                "asset not found",
                format!("stored file for {filename} is unreadable"),
            )
        }
    };

    let range_header = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    match range_header {
        None => {
            let body = ranged_body(file, 0, size).await;
            build_response(StatusCode::OK, mime, size, None, body)
        }
        Some(range) => match parse_byte_range(range, size) {
            Ok(RangeSpec::Partial(ByteRange { start, end })) => {
                let len = end - start + 1;
                let body = ranged_body(file, start, len).await;
                build_response(
                    StatusCode::PARTIAL_CONTENT,
                    mime,
                    len,
                    Some(format!("bytes {start}-{end}/{size}")),
                    body,
                )
            }
            Ok(RangeSpec::Full) => {
                let body = ranged_body(file, 0, size).await;
                build_response(StatusCode::OK, mime, size, None, body)
            }
            Err(RangeError) => {
                let mut resp = problem(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    "range not satisfiable",
                    format!("invalid byte range: {range}"),
                );
                if let Ok(v) = format!("bytes */{size}").parse() {
                    resp.headers_mut().insert(header::CONTENT_RANGE, v);
                }
                resp
            }
        },
    }
}

/// Open the file, seek to `start`, and stream exactly `len` bytes.
async fn ranged_body(mut file: tokio::fs::File, start: u64, len: u64) -> Body {
    if start > 0 {
        let _ = file.seek(SeekFrom::Start(start)).await;
    }
    let limited = file.take(len);
    Body::from_stream(ReaderStream::new(limited))
}

/// Assemble a streamed response with the standard asset headers.
fn build_response(
    status: StatusCode,
    mime: String,
    content_length: u64,
    content_range: Option<String>,
    body: Body,
) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, CACHE_CONTROL_IMMUTABLE)
        .header(header::CONTENT_LENGTH, content_length.to_string());
    if let Some(content_range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, content_range);
    }
    builder.body(body).expect("asset response is valid")
}

/// Fallback content type from the filename extension (assets without a stored
/// mime, e.g. a poster that predates mime stamping).
fn mime_for_extension(filename: &str) -> String {
    let ext = filename.rsplit('.').next().unwrap_or("");
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "vtt" => "text/vtt",
        _ => "application/octet-stream",
    }
    .to_string()
}
