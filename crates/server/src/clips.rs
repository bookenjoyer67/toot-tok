//! GET /api/v1/clips/{id} — clip metadata with its media asset list.

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use toottok_db::clip::Clip;
use toottok_db::media_asset::MediaAsset;

use crate::problem::problem;
use crate::AppState;

pub async fn show(State(state): State<AppState>, Path(id): Path<i64>) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };

    let clip = match Clip::fetch_by_id(pool, id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return problem(
                StatusCode::NOT_FOUND,
                "clip not found",
                format!("no clip with id {id}"),
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

    let assets = match MediaAsset::fetch_for_clip(pool, id).await {
        Ok(a) => a,
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
        }
    };
    let assets: Vec<Value> = assets
        .iter()
        .map(|a| {
            let filename = a.path.rsplit('/').next().unwrap_or(&a.path);
            json!({
                "kind": a.kind,
                "rendition": a.rendition,
                "url": format!("/assets/{id}/{filename}"),
            })
        })
        .collect();

    let body = json!({
        "id": clip.id,
        "status": clip.status,
        "duration_s": clip.duration_s,
        "width": clip.width,
        "height": clip.height,
        "like_count": clip.like_count,
        "comment_count": clip.comment_count,
        "share_count": clip.share_count,
        "view_count": clip.view_count,
        "caption_html": clip.caption_html,
        "created_at": clip.created_at,
        "assets": assets,
    });

    (
        [(header::CONTENT_TYPE, "application/json".to_string())],
        axum::body::Body::from(serde_json::to_vec(&body).expect("clip metadata serializes")),
    )
        .into_response()
}
