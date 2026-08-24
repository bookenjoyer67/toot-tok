//! toottok-federation — ActivityPub types, inbox processing, egress-guarded
//! delivery, and the crate (`activitypub_federation`) wiring.

pub mod activity;
pub mod data;
pub mod deliver;
pub mod egress;
pub mod error;
pub mod note;
pub mod object;
pub mod resolver;

pub use activitypub_federation;
pub use data::FederationData;
pub use egress::EgressGuard;
pub use error::Error;

use activitypub_federation::config::{FederationConfig, FederationMiddleware};
use serde_json::{json, Value};
use sqlx::PgPool;
use toottok_db::actor::Actor;
use tracing::info;
use url::Url;

use crate::object::DbActor;

/// Build the crate's `FederationConfig`, signing fetch requests with the local
/// instance actor (`Application` at `/ap/actor`, created by the server at
/// startup). `debug` mode (allowing http + localhost) is enabled when the
/// egress guard allows loopback — i.e. the dev/test rig only.
pub async fn build_config(data: FederationData) -> Result<FederationConfig<FederationData>, Error> {
    let instance_ap = format!("{}/ap/actor", data.base_url);
    let instance_row = Actor::fetch_by_ap_id(&data.pool, &instance_ap)
        .await?
        .ok_or_else(|| Error::Other(format!("instance actor {instance_ap} not found")))?;
    let instance = DbActor::from_row(instance_row);

    let config = FederationConfig::builder()
        .domain(data.domain.clone())
        .signed_fetch_actor(&instance)
        // Draft-10 signature shape ((request-target) host date, no
        // (created)/(expires) pseudo-headers) — what Loops' verifier and most
        // PHP/legacy fediverse software actually check.
        .http_signature_compat(true)
        .app_data(data.clone())
        .debug(data.allow_loopback)
        // Route the crate's OWN outbound fetches (signature keyId derefs,
        // ObjectId resolution) through the same DNS-level egress policy as
        // our delivery client — no bypass path remains.
        .client(reqwest_middleware::ClientWithMiddleware::from(
            crate::resolver::guarded_client(
                data.allow_loopback,
                if data.allow_loopback {
                    vec![data.domain.clone(), "localhost".to_string()]
                } else {
                    vec![]
                },
            ),
        ))
        .build()
        .await?;
    info!(domain = %data.domain, "federation config built");
    Ok(config)
}

/// Axum middleware wrapper carrying the config into handlers.
pub type FedMiddleware = FederationMiddleware<FederationData>;

/// Egress-guarded GET of a remote object (used for outbound actor resolution).
/// Verifies the response `id` matches the requested URL (mirrors the crate's
/// own fetch checks so a redirect/host mismatch cannot smuggle a different
/// object).
pub async fn fetch_remote_object_json(guard: &EgressGuard, url: &Url) -> Result<Value, Error> {
    let client = guard.client_for(url).await?;
    let resp = client
        .get(url.as_str())
        .header("Accept", activitypub_federation::FEDERATION_CONTENT_TYPE)
        .send()
        .await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(Error::Other(format!(
            "remote fetch {url} failed with {status}"
        )));
    }
    let body = resp.bytes().await?;
    let value: Value = serde_json::from_slice(&body)?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    if id != url.as_str() {
        return Err(Error::Other(format!(
            "remote fetch {url} returned object with id {id}"
        )));
    }
    Ok(value)
}

/// Fetch + cache a remote actor row from its ActivityPub URL (egress-guarded),
/// recording instances bookkeeping on success.
pub async fn fetch_remote_actor(
    pool: &PgPool,
    guard: &EgressGuard,
    url: &Url,
) -> Result<Actor, Error> {
    let value = fetch_remote_object_json(guard, url).await?;
    let parts = activity::parse_remote_actor_json(&value)?;
    let row = Actor::upsert_remote(
        pool,
        &parts.username,
        &parts.domain,
        &parts.actor_type,
        &parts.public_key_pem,
        &parts.inbox,
        parts.shared_inbox.as_deref(),
        &parts.outbox,
        &parts.followers,
        &parts.id,
    )
    .await?;
    let _ = toottok_db::instance::Instance::upsert_success(
        pool,
        &parts.domain,
        None,
        None,
        &parts.inbox,
    )
    .await;
    Ok(row)
}

/// Split a remote handle into `(name, domain)`. Accepts `user@domain`,
/// `@user@domain`, and (defensively) `@user@domain@extra` by taking only the
/// first `@` as the separator. Returns `None` for bare usernames, empty
/// parts, or non-`@` queries — those stay local-style.
fn parse_remote_handle(handle: &str) -> Option<(&str, &str)> {
    let raw = handle.trim().trim_start_matches('@');
    let (name, domain) = raw.split_once('@')?;
    let domain = domain.trim();
    if name.is_empty() || domain.is_empty() {
        return None;
    }
    Some((name, domain))
}

/// Resolve a remote actor from a handle (`user@domain`, with optional leading
/// `@` or full `@user@domain`, or a bare `user@domain`), via WebFinger. On a
/// hit this fetches + caches the actor (egress-guarded) and returns its row —
/// the same path `POST /follows` uses. Returns `Ok(None)` for a handle that
/// does not look remote or that cannot be resolved.
pub async fn resolve_remote_actor_by_handle(
    pool: &PgPool,
    guard: &EgressGuard,
    handle: &str,
) -> Result<Option<Actor>, Error> {
    let Some((name, domain)) = parse_remote_handle(handle) else {
        return Ok(None);
    };

    let webfinger = format!(
        "https://{domain}/.well-known/webfinger?resource=acct:{name}@{domain}"
    );
    let url = match Url::parse(&webfinger) {
        Ok(u) => u,
        Err(_) => return Ok(None),
    };
    if url.scheme() != "https" {
        return Ok(None);
    }

    let client = match guard.client_for(&url).await {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let resp = match client
        .get(url.as_str())
        .header("accept", "application/jrd+json, application/json")
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        _ => return Ok(None),
    };
    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return Ok(None),
    };
    let links = body
        .get("links")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Standard WebFinger `self` link carries the ActivityPub profile URL.
    for link in links {
        if link.get("rel").and_then(Value::as_str) != Some("self") {
            continue;
        }
        let Some(href) = link.get("href").and_then(Value::as_str) else {
            continue;
        };
        let Ok(actor_url) = Url::parse(href) else {
            continue;
        };
        if actor_url.scheme() == "https" || url.host_str() == Some("localhost") {
            match fetch_remote_actor(pool, guard, &actor_url).await {
                Ok(a) => return Ok(Some(a)),
                Err(_) => return Ok(None),
            }
        }
    }
    Ok(None)
}

/// ── HTTP document builders (webfinger / nodeinfo / collections) ─────────────
/// JRD for `/.well-known/webfinger`: resolve to `url`, advertising both an HTML
/// profile page and the ActivityPub `self` link.
pub fn webfinger_jrd(subject: &str, url: &Url) -> Value {
    let wf = activitypub_federation::fetch::webfinger::build_webfinger_response(
        subject.to_string(),
        url.clone(),
    );
    serde_json::to_value(wf).expect("webfinger serializes")
}

/// JRD for `/.well-known/nodeinfo`: advertise both the 2.0 and 2.1 documents.
pub fn nodeinfo_jrd(base_url: &str) -> Value {
    json!({
        "links": [
            {
                "rel": "http://nodeinfo.diaspora.software/ns/schema/2.0",
                "href": format!("{base_url}/nodeinfo/2.0"),
            },
            {
                "rel": "http://nodeinfo.diaspora.software/ns/schema/2.1",
                "href": format!("{base_url}/nodeinfo/2.1"),
            },
        ]
    })
}

/// A NodeInfo 2.x document. `version` is `"2.0"` or `"2.1"`.
pub fn nodeinfo_document(base_url: &str, version: &str, users: i64, posts: i64) -> Value {
    json!({
        "version": version,
        "software": {
            "name": "toottok",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "protocols": ["activitypub"],
        "services": { "inbound": [], "outbound": [] },
        "openRegistrations": true,
        "usage": { "users": { "total": users }, "localPosts": posts },
        "metadata": { "nodeName": base_url },
    })
}

/// A minimal paged `OrderedCollection` (followers/following/outbox).
pub fn ordered_collection_page(id: &Url, total_items: usize, items: Vec<Value>) -> Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": id,
        "type": "OrderedCollection",
        "totalItems": total_items,
        "first": {
            "id": format!("{}?page=1", id),
            "type": "OrderedCollectionPage",
            "partOf": id,
            "orderedItems": items,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::parse_remote_handle;

    #[test]
    fn parses_remote_handles() {
        assert_eq!(
            parse_remote_handle("dansup@loops.video"),
            Some(("dansup", "loops.video"))
        );
        assert_eq!(
            parse_remote_handle("@dansup@loops.video"),
            Some(("dansup", "loops.video"))
        );
        assert_eq!(
            parse_remote_handle("  @dansup@loops.video  "),
            Some(("dansup", "loops.video"))
        );
        // First `@` separates name from domain; the rest stays in domain.
        assert_eq!(
            parse_remote_handle("@a@b@c"),
            Some(("a", "b@c"))
        );
    }

    #[test]
    fn rejects_non_remote_handles() {
        assert_eq!(parse_remote_handle("dansup"), None);
        assert_eq!(parse_remote_handle("@dansup"), None);
        assert_eq!(parse_remote_handle("@"), None);
        assert_eq!(parse_remote_handle("@dansup@"), None);
        assert_eq!(parse_remote_handle("@dansup@   "), None);
        assert_eq!(parse_remote_handle(""), None);
    }
}

