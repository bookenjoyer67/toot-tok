//! Server-to-server federation endpoints + the outbound follow API.
//!
//! AP endpoints live under the `FederationMiddleware` layer so handlers can take
//! `Data<FederationData>`. Inbox POSTs go through the crate's `receive_activity`
//! (HTTP signature verification + typed dispatch) which runs the STRICT inbound
//! pipeline (idempotency → tombstone → store → process → stamp) inside the
//! activity handlers.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use toottok_db::activity::Activity as ActivityRow;
use toottok_db::actor::Actor as DbActorRow;
use toottok_db::clip::Clip as ClipRow;
use toottok_db::follow::Follow;
use toottok_federation::activity::{
    activity_id_from_json, follow_activity, undo_activity, ApActivities,
};
use toottok_federation::activitypub_federation::axum::inbox::{receive_activity, ActivityData};
use toottok_federation::activitypub_federation::axum::json::FederationJson;
use toottok_federation::activitypub_federation::config::Data;
use toottok_federation::activitypub_federation::fetch::webfinger::{
    build_webfinger_response, extract_webfinger_name,
};
use toottok_federation::activitypub_federation::protocol::context::WithContext;
use toottok_federation::data::FederationData;
use toottok_federation::deliver::{
    enqueue_delivery, enqueue_follow_delivery, shared_inbox_or_inbox,
};
use toottok_federation::object::DbActor;
use url::Url;

use crate::problem::problem;
use crate::session::AuthUser;
use crate::AppState;

/// GET /.well-known/webfinger?resource=acct:user@domain — users and the
/// instance actor (`acct:domain@domain`).
pub async fn webfinger(
    State(state): State<AppState>,
    data: Data<FederationData>,
    Query(query): Query<WebfingerQuery>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let name = match extract_webfinger_name(&query.resource, &data) {
        Ok(n) => n,
        Err(e) => return problem(StatusCode::NOT_FOUND, "webfinger", e.to_string()),
    };

    // Local user first.
    if let Ok(Some(actor)) = DbActorRow::fetch_by_username_local(pool, name).await {
        let id = Url::parse(&actor.ap_id).expect("stored ap_id is valid");
        return Json(build_webfinger_response(query.resource.clone(), id)).into_response();
    }
    // Instance actor: acct:{domain}@{domain}.
    if name == data.domain.split(':').next().unwrap_or(&data.domain) {
        let instance_ap = format!("{}/ap/actor", data.base_url);
        if let Ok(Some(actor)) = DbActorRow::fetch_by_ap_id(pool, &instance_ap).await {
            let id = Url::parse(&actor.ap_id).expect("stored ap_id is valid");
            return Json(build_webfinger_response(query.resource.clone(), id)).into_response();
        }
    }
    problem(
        StatusCode::NOT_FOUND,
        "webfinger",
        format!("no such actor: {}", query.resource),
    )
    .into_response()
}

#[derive(Deserialize)]
pub struct WebfingerQuery {
    resource: String,
}

/// GET /ap/actor — the instance actor (`Application`).
pub async fn instance_actor(
    State(state): State<AppState>,
    data: Data<FederationData>,
    headers: HeaderMap,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if !accepts_activity_json(&headers) {
        return problem(
            StatusCode::NOT_ACCEPTABLE,
            "not acceptable",
            "client must accept application/activity+json",
        );
    }
    let instance_ap = format!("{}/ap/actor", data.base_url);
    let Some(row) = DbActorRow::fetch_by_ap_id(pool, &instance_ap)
        .await
        .ok()
        .flatten()
    else {
        return problem(StatusCode::NOT_FOUND, "actor", "instance actor not found");
    };
    FederationJson(DbActor::from_row(row).to_json()).into_response()
}

/// GET /users/{username} — a local person as ActivityPub JSON.
pub async fn user_actor(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if !accepts_activity_json(&headers) {
        return problem(
            StatusCode::NOT_ACCEPTABLE,
            "not acceptable",
            "client must accept application/activity+json",
        );
    }
    let Some(row) = DbActorRow::fetch_by_username_local(pool, &username)
        .await
        .ok()
        .flatten()
    else {
        return problem(
            StatusCode::NOT_FOUND,
            "actor",
            format!("no such user: {username}"),
        );
    };
    if row.deleted_at.is_some() {
        return problem(
            StatusCode::NOT_FOUND,
            "actor",
            format!("no such user: {username}"),
        );
    }
    FederationJson(DbActor::from_row(row).to_json()).into_response()
}

/// GET /users/{u}/followers|following|outbox — minimal paged OrderedCollection.
pub async fn user_collection(
    State(state): State<AppState>,
    data: Data<FederationData>,
    Path((username, collection)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if !accepts_activity_json(&headers) {
        return problem(
            StatusCode::NOT_ACCEPTABLE,
            "not acceptable",
            "client must accept application/activity+json",
        );
    }
    let Some(actor) = DbActorRow::fetch_by_username_local(pool, &username)
        .await
        .ok()
        .flatten()
    else {
        return problem(
            StatusCode::NOT_FOUND,
            "actor",
            format!("no such user: {username}"),
        );
    };
    let collection_url = Url::parse(&format!("{}/users/{username}/{collection}", data.base_url))
        .expect("collection url parses");

    let items: Vec<Value> = match collection.as_str() {
        "followers" => match Follow::follower_actor_ids(pool, actor.id).await {
            Ok(ids) => ap_id_list(pool, &ids).await,
            Err(_) => vec![],
        },
        "following" => match Follow::following_actor_ids(pool, actor.id).await {
            Ok(ids) => ap_id_list(pool, &ids).await,
            Err(_) => vec![],
        },
        "outbox" => vec![], // clips federate in wave B
        other => {
            return problem(
                StatusCode::NOT_FOUND,
                "collection",
                format!("no such collection: {other}"),
            )
            .into_response()
        }
    };

    let total = items.len();
    FederationJson(toottok_federation::ordered_collection_page(
        &collection_url,
        total,
        items,
    ))
    .into_response()
}

/// Resolve a list of actor ids to their ActivityPub `ap_id` strings.
async fn ap_id_list(pool: &sqlx::PgPool, ids: &[i64]) -> Vec<Value> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        if let Ok(Some(a)) = DbActorRow::fetch_by_id(pool, *id).await {
            out.push(Value::String(a.ap_id));
        }
    }
    out
}

/// Which AP document to serve for a clip.
enum ClipApShape {
    /// The bare `Note` object (GET /clips/{id}).
    Note,
    /// The `Create(Note)` wrapper (GET /clips/{id}/activity).
    Create,
}

/// GET /clips/{id} — the clip as a Loops-shaped `Note`, served ONLY on
/// `Accept: application/activity+json` (every other Accept keeps the
/// non-AP response). GET /clips/{id}/activity serves the same clip wrapped
/// in its `Create`.
pub async fn clip_object(
    State(state): State<AppState>,
    data: Data<FederationData>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Response {
    serve_clip_ap(state, data, id, headers, ClipApShape::Note).await
}

/// GET /clips/{id}/activity — the `Create(Note)` wrapper.
pub async fn clip_activity(
    State(state): State<AppState>,
    data: Data<FederationData>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Response {
    serve_clip_ap(state, data, id, headers, ClipApShape::Create).await
}

async fn serve_clip_ap(
    state: AppState,
    data: Data<FederationData>,
    id: i64,
    headers: HeaderMap,
    shape: ClipApShape,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    if !accepts_activity_json(&headers) {
        return problem(
            StatusCode::NOT_ACCEPTABLE,
            "not acceptable",
            "client must accept application/activity+json",
        );
    }

    let Ok(Some(clip)) = ClipRow::fetch_by_id(pool, id).await else {
        return problem(
            StatusCode::NOT_FOUND,
            "clip not found",
            format!("no clip with id {id}"),
        );
    };
    if clip.deleted_at.is_some() {
        return problem(
            StatusCode::NOT_FOUND,
            "clip not found",
            format!("no clip with id {id}"),
        );
    }
    // Only finalized local rows are served here: the canonical ap_id
    // ({base}/clips/{id}) is stamped at finalize; pending uploads and cached
    // remote rows have foreign ids that would fail fetch-side verification.
    if clip.ap_id != toottok_federation::note::clip_object_id(&data.base_url, id) {
        return problem(
            StatusCode::NOT_FOUND,
            "clip not found",
            format!("no clip with id {id}"),
        );
    }
    let author = match DbActorRow::fetch_by_id(pool, clip.actor_id).await {
        Ok(Some(a)) => a,
        _ => {
            return problem(
                StatusCode::NOT_FOUND,
                "clip not found",
                format!("clip {id} has no author"),
            )
        }
    };

    // F5: advertise only renditions that actually exist — a sub-720p source
    // must not federate a 720.mp4 URL that 404s.
    let media_filename = match state.pool.as_ref() {
        Some(p) => toottok_db::media_asset::MediaAsset::largest_video_filename(p, clip.id)
            .await
            .ok()
            .flatten(),
        None => None,
    }
    .or_else(|| Some("720.mp4".to_string()));

    let document = match shape {
        ClipApShape::Note => toottok_federation::note::clip_note_json(
            &data.base_url,
            &clip,
            &author,
            media_filename.as_deref(),
        ),
        ClipApShape::Create => toottok_federation::note::clip_create_activity(
            &data.base_url,
            &clip,
            &author,
            media_filename.as_deref(),
        ),
    };
    FederationJson(WithContext::new_default(document)).into_response()
}

/// POST /ap/inbox (shared inbox) and POST /users/{u}/inbox.
pub async fn inbox(data: Data<FederationData>, activity_data: ActivityData) -> Response {
    match receive_activity::<WithContext<ApActivities>, DbActor, FederationData>(
        activity_data,
        &data,
    )
    .await
    {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(e) => e.into_response(),
    }
}

/// GET /.well-known/nodeinfo — JRD advertising both 2.0 and 2.1.
pub async fn nodeinfo_jrd(data: Data<FederationData>) -> Response {
    Json(toottok_federation::nodeinfo_jrd(&data.base_url)).into_response()
}

/// GET /nodeinfo/{version} (2.0 or 2.1).
pub async fn nodeinfo_doc(
    State(state): State<AppState>,
    data: Data<FederationData>,
    Path(version): Path<String>,
) -> Response {
    if version != "2.0" && version != "2.1" {
        return problem(
            StatusCode::NOT_FOUND,
            "nodeinfo",
            format!("unsupported nodeinfo version: {version}"),
        )
        .into_response();
    }
    let Some(pool) = &state.pool else {
        return Json(toottok_federation::nodeinfo_document(
            &data.base_url,
            &version,
            0,
            0,
        ))
        .into_response();
    };
    let users: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users u JOIN actors a ON a.id = u.actor_id WHERE u.deleted_at IS NULL AND u.status = 'active' AND a.suspended_at IS NULL",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    let posts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM clips WHERE deleted_at IS NULL AND status = 'ready'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);
    Json(toottok_federation::nodeinfo_document(
        &data.base_url,
        &version,
        users,
        posts,
    ))
    .into_response()
}

/// ── outbound follow API ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct FollowRequest {
    /// Full ActivityPub actor URI to follow, e.g. `https://b.test/users/bob`.
    actor_uri: String,
}

/// POST /api/v1/follows — create/refresh the remote actor (egress-guarded
/// fetch), insert `follows` (state=requested) and enqueue the signed Follow
/// delivery job.
pub async fn api_follow(
    State(state): State<AppState>,
    data: Data<FederationData>,
    auth: AuthUser,
    body: Json<FollowRequest>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let url = match Url::parse(&body.actor_uri) {
        Ok(u) => u,
        Err(e) => return problem(StatusCode::BAD_REQUEST, "invalid actor_uri", e.to_string()),
    };
    if url.scheme() != "https" && url.scheme() != "http" {
        return problem(
            StatusCode::BAD_REQUEST,
            "invalid actor_uri",
            "actor_uri must be an http(s) URL",
        );
    }

    let local_target = toottok_federation::activity::is_local_url(&url, &data.domain);
    let remote = if local_target {
        // Same-instance follow: resolve the row directly. Routing this through
        // fetch_remote_actor would run the REMOTE upsert against the local
        // actor's ap_id and stamp a non-NULL domain onto the row, which hides
        // the profile (every local-profile query filters domain IS NULL).
        let username = url.path().rsplit('/').next().unwrap_or("");
        match DbActorRow::fetch_by_username_local(pool, username).await {
            Ok(Some(a)) => a,
            Ok(None) => {
                return problem(
                    StatusCode::BAD_REQUEST,
                    "actor not found",
                    format!("no local actor at {url}"),
                )
            }
            Err(e) => {
                return problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "database error",
                    format!("{e}"),
                )
                .into_response()
            }
        }
    } else {
        match toottok_federation::fetch_remote_actor(pool, &state.egress, &url).await {
            Ok(r) => r,
            Err(e) => {
                return problem(
                    StatusCode::BAD_GATEWAY,
                    "actor fetch failed",
                    format!("could not fetch remote actor: {e}"),
                )
                .into_response()
            }
        }
    };
    if remote.id == auth.actor.id {
        return problem(
            StatusCode::BAD_REQUEST,
            "cannot follow self",
            "you cannot follow yourself",
        );
    }

    let activity = follow_activity(&data.base_url, &auth.actor.ap_id, &remote.ap_id);
    let follow_id = activity_id_from_json(&activity);

    // A same-instance follow never crosses the network: delivering the Follow
    // to our own inbox would trip the egress guard's local-URL check, so
    // record it as accepted right away and skip delivery.
    let initial_state = if local_target {
        "accepted"
    } else {
        "requested"
    };

    match Follow::upsert(
        pool,
        auth.actor.id,
        remote.id,
        Some(&follow_id),
        initial_state,
    )
    .await
    {
        Ok(_) => {}
        Err(e) => {
            return problem(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database error",
                format!("{e}"),
            )
            .into_response()
        }
    }
    let _ = ActivityRow::create_outbound(
        pool,
        &follow_id,
        &auth.actor.ap_id,
        Some(&remote.ap_id),
        &activity,
    )
    .await;
    if !local_target {
        match enqueue_follow_delivery(pool, auth.actor.id, remote.id, &activity).await {
            Ok(_) => {}
            Err(e) => {
                return problem(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "job error",
                    format!("{e}"),
                )
                .into_response()
            }
        }
    }

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "follower": auth.actor.ap_id,
            "target": remote.ap_id,
            "target_actor_id": remote.id,
            "state": initial_state,
            "activity_id": follow_id,
        })),
    )
        .into_response()
}

/// POST /api/v1/follows/{target_actor_id}/unfollow — drop the follow locally and
/// deliver `Undo(Follow)` to the target.
pub async fn api_unfollow(
    State(state): State<AppState>,
    data: Data<FederationData>,
    auth: AuthUser,
    Path(target_id): Path<i64>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let Some(follow) = Follow::fetch_by_pair(pool, auth.actor.id, target_id)
        .await
        .ok()
        .flatten()
    else {
        return problem(StatusCode::NOT_FOUND, "follow", "no such follow relation");
    };
    let Some(follow_id) = follow.ap_activity_id.as_deref() else {
        return problem(StatusCode::CONFLICT, "follow", "follow has no activity id");
    };

    Follow::delete(pool, auth.actor.id, target_id).await.ok();

    let activity = undo_activity(&data.base_url, &auth.actor.ap_id, follow_id);
    let undo_id = activity_id_from_json(&activity);
    let _ = ActivityRow::create_outbound(
        pool,
        &undo_id,
        &auth.actor.ap_id,
        Some(follow_id),
        &activity,
    )
    .await;

    let target = match DbActorRow::fetch_by_id(pool, target_id).await {
        Ok(Some(t)) => t,
        _ => return StatusCode::NO_CONTENT.into_response(),
    };
    let inbox = shared_inbox_or_inbox(&target);
    match enqueue_delivery(pool, auth.actor.id, &inbox, &activity).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "job error",
            format!("{e}"),
        )
        .into_response(),
    }
}

/// GET /api/v1/follows/mine — actor ids + handles this user follows
/// (state accepted). Lets the web UI hydrate follow-button state in one call.
pub async fn api_my_follows(
    State(state): State<AppState>,
    data: Data<FederationData>,
    auth: AuthUser,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let _ = &data;
    match Follow::following_rows(pool, auth.actor.id).await {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|(actor_id, username, domain)| {
                    json!({ "actor_id": actor_id, "username": username, "domain": domain })
                })
                .collect();
            Json(json!({ "following": items })).into_response()
        }
        Err(e) => problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            "database error",
            format!("{e}"),
        ),
    }
}

/// GET /api/v1/profiles/{username}/follow-state — is the caller following the
/// named local profile? Cheap check for the profile page's Follow button.
pub async fn api_follow_state(
    State(state): State<AppState>,
    data: Data<FederationData>,
    auth: AuthUser,
    Path(username): Path<String>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let _ = &data;
    let target = match DbActorRow::fetch_by_username_local(pool, &username).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            return problem(StatusCode::NOT_FOUND, "profile not found", format!("no actor named @{username}"))
        }
        Err(e) => {
            return problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}"))
        }
    };
    let state_str = match Follow::fetch_by_pair(pool, auth.actor.id, target.id).await {
        Ok(Some(f)) => f.state,
        Ok(None) => "none".to_string(),
        Err(e) => {
            return problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}"))
        }
    };
    Json(json!({ "target_actor_id": target.id, "state": state_str })).into_response()
}

/// GET /api/v1/profiles/{username}/{list} — followers | following handles
/// (local actors only, newest first, capped at 200).
pub async fn api_follow_list(
    State(state): State<AppState>,
    data: Data<FederationData>,
    auth: AuthUser,
    Path((username, list)): Path<(String, String)>,
) -> Response {
    let Some(pool) = &state.pool else {
        return problem(
            StatusCode::SERVICE_UNAVAILABLE,
            "database unavailable",
            "database is not configured",
        );
    };
    let _ = (&data, auth);
    if !matches!(list.as_str(), "followers" | "following") {
        return problem(StatusCode::NOT_FOUND, "list", "use followers or following");
    }
    let target = match DbActorRow::fetch_by_username_local(pool, &username).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            return problem(
                StatusCode::NOT_FOUND,
                "profile not found",
                format!("no actor named @{username}"),
            )
        }
        Err(e) => return problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}")),
    };
    let rows = if list == "followers" {
        Follow::follower_rows(pool, target.id, 200).await
    } else {
        Follow::following_list(pool, target.id, 200).await
    };
    match rows {
        Ok(rows) => {
            let items: Vec<Value> = rows
                .into_iter()
                .map(|(actor_id, username)| json!({ "actor_id": actor_id, "username": username }))
                .collect();
            Json(json!({ "items": items })).into_response()
        }
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, "database error", format!("{e}")),
    }
}

/// True when the request's `Accept` header permits an ActivityPub JSON payload.
fn accepts_activity_json(headers: &HeaderMap) -> bool {
    let Some(accept) = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    accept.contains("*/*")
        || accept.contains("application/activity+json")
        || accept.contains("application/ld+json")
}
