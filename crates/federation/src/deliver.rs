//! Outbound delivery: HTTP-signature signing + egress-guarded POST.
//!
//! Signing mirrors the crate's own `sign_request` (draft-cavage rsa-sha256,
//! `(request-target) content-type date digest host`, keyId `{actor}#main-key`)
//! so inbound signature verification on the remote side accepts our requests.
//!
//! Delivery goes through [`crate::egress::EgressGuard::client_for`]: the inbox
//! host is resolved once, validated, and pinned into a reqwest client, so the
//! connection can never re-resolve to a different address.

use std::sync::LazyLock;
use std::time::{Duration, SystemTime};

use base64::engine::general_purpose::STANDARD as Base64;
use base64::Engine as _;
use bytes::Bytes;
use http::header::{HeaderName, HeaderValue};
use http_signature_normalization_reqwest::prelude::{Config, SignExt};
use http_signature_normalization_reqwest::DefaultSpawner;
use httpdate::fmt_http_date;
use reqwest::Request;
use reqwest_middleware::{ClientWithMiddleware, RequestBuilder};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::Pkcs1v15Sign;
use rsa::RsaPrivateKey;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use toottok_db::actor::Actor;
use toottok_db::instance::Instance;
use toottok_db::job::Job;
use tracing::{debug, info, warn};
use url::Url;

use crate::egress::EgressGuard;
use crate::error::Error;

/// Signature is valid for one hour (matches the crate's `EXPIRES_AFTER`).
const EXPIRES_AFTER: Duration = Duration::from_secs(60 * 60);

static SIGN_CONFIG: LazyLock<Config<DefaultSpawner>> =
    LazyLock::new(|| Config::new().set_expiration(EXPIRES_AFTER));

/// Sign `body` as a POST from `actor_id` (using its private key). The returned
/// request carries `Signature`, `Digest`, `Date`, `Host`, and `Content-Type`.
pub async fn sign_request(
    request_builder: RequestBuilder,
    actor_id: &Url,
    body: Bytes,
    private_key: RsaPrivateKey,
) -> Result<Request, Error> {
    let key_id = format!("{actor_id}#main-key");
    request_builder
        .signature_with_digest(
            SIGN_CONFIG.clone(),
            key_id,
            Sha256::new(),
            body,
            move |signing_string| {
                Ok(Base64.encode(private_key.sign(
                    Pkcs1v15Sign::new::<Sha256>(),
                    &Sha256::digest(signing_string.as_bytes()),
                )?)) as Result<_, Error>
            },
        )
        .await
}

/// Outcome of one delivery attempt. `Rejected` (permanent client error) is not
/// retried; `Failed` is retried with backoff by the worker.
#[derive(Debug)]
pub enum DeliverOutcome {
    Delivered,
    Rejected(String),
    Failed(String),
}

/// Core send: pin + validate the inbox, sign the activity with `signer`'s
/// private key, POST it, and record instances bookkeeping.
pub async fn deliver_activity(
    pool: &PgPool,
    guard: &EgressGuard,
    signer: &Actor,
    inbox_url: &Url,
    activity: &Value,
) -> Result<DeliverOutcome, Error> {
    let private_key = {
        let pem = signer
            .private_key_pem
            .as_deref()
            .ok_or_else(|| Error::Other(format!("actor {} has no private key", signer.ap_id)))?;
        RsaPrivateKey::from_pkcs8_pem(pem)
            .map_err(|e| Error::Other(format!("private key decode failed: {e}")))?
    };

    let client = guard.client_for(inbox_url).await?;

    let mut headers = http::HeaderMap::new();
    headers.insert(
        HeaderName::from_static("content-type"),
        HeaderValue::from_static(activitypub_federation::FEDERATION_CONTENT_TYPE),
    );
    let host = match inbox_url.host_str() {
        Some(h) if inbox_url.port().is_some() => format!("{h}:{}", inbox_url.port().unwrap()),
        Some(h) => h.to_string(),
        None => return Err(Error::Other(format!("inbox {inbox_url} has no host"))),
    };
    headers.insert(
        HeaderName::from_static("host"),
        HeaderValue::from_str(&host).expect("host header is valid"),
    );
    headers.insert(
        "date",
        HeaderValue::from_str(&fmt_http_date(SystemTime::now())).expect("date header is valid"),
    );

    let body = serde_json::to_vec(activity)?;
    let signer_id = Url::parse(&signer.ap_id)?;
    let request_builder = ClientWithMiddleware::from(client.clone())
        .post(inbox_url.as_str())
        .headers(headers);
    let signed = sign_request(request_builder, &signer_id, Bytes::from(body), private_key).await?;

    debug!(inbox = %inbox_url, "delivering signed activity");
    let response = client.execute(signed).await;

    let domain = inbox_url.host_str().unwrap_or_default().to_string();
    match response {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                let _ =
                    Instance::upsert_success(pool, &domain, None, None, inbox_url.as_str()).await;
                info!(inbox = %inbox_url, status = %status, "activity delivered");
                Ok(DeliverOutcome::Delivered)
            } else if status.is_client_error()
                && status != http::StatusCode::REQUEST_TIMEOUT
                && status != http::StatusCode::TOO_MANY_REQUESTS
            {
                let text = resp.text().await.unwrap_or_default();
                // Bookkeeping on EVERY contact: a remote actively rejecting us
                // is still an instance we talked to.
                let _ = Instance::record_failure(pool, &domain, inbox_url.as_str()).await;
                warn!(inbox = %inbox_url, status = %status, "delivery rejected permanently: {text}");
                Ok(DeliverOutcome::Rejected(text))
            } else {
                let text = resp.text().await.unwrap_or_default();
                let _ = Instance::record_failure(pool, &domain, inbox_url.as_str()).await;
                warn!(inbox = %inbox_url, status = %status, "delivery failed transiently: {text}");
                Ok(DeliverOutcome::Failed(format!("status {status}: {text}")))
            }
        }
        Err(e) => {
            let _ = Instance::record_failure(pool, &domain, inbox_url.as_str()).await;
            warn!(inbox = %inbox_url, error = %e, "delivery failed");
            Ok(DeliverOutcome::Failed(e.to_string()))
        }
    }
}

/// Enqueue a generic signed delivery (e.g. Accept-back) for the worker.
/// `actor_id` is the DB id of the signing local actor.
pub async fn enqueue_delivery(
    pool: &PgPool,
    actor_id: i64,
    inbox_url: &str,
    activity: &Value,
) -> Result<(), Error> {
    let payload = serde_json::json!({
        "actor_id": actor_id,
        "inbox_url": inbox_url,
        "activity": activity,
    });
    Job::create(pool, "deliver", &payload, None).await?;
    Ok(())
}

/// Enqueue an outbound follow delivery. The worker resolves the target actor's
/// inbox (shared-inbox preferred) from `target_actor_id`.
pub async fn enqueue_follow_delivery(
    pool: &PgPool,
    follower_actor_id: i64,
    target_actor_id: i64,
    activity: &Value,
) -> Result<(), Error> {
    let payload = serde_json::json!({
        "follower_actor_id": follower_actor_id,
        "target_actor_id": target_actor_id,
        "activity": activity,
    });
    Job::create(pool, "deliver_follow", &payload, None).await?;
    Ok(())
}

/// Process one `deliver` / `deliver_follow` job: resolve the signer + inbox,
/// sign and send via the egress guard, do instances bookkeeping, and return the
/// outcome so the worker can mark done / schedule a retry / dead-letter.
pub async fn deliver_job(
    pool: &PgPool,
    guard: &EgressGuard,
    job: &Job,
) -> Result<DeliverOutcome, Error> {
    match job.kind.as_str() {
        "deliver_follow" => {
            let follower_actor_id = job
                .payload
                .get("follower_actor_id")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    Error::Other("deliver_follow payload missing follower_actor_id".into())
                })?;
            let target_actor_id = job
                .payload
                .get("target_actor_id")
                .and_then(Value::as_i64)
                .ok_or_else(|| {
                    Error::Other("deliver_follow payload missing target_actor_id".into())
                })?;
            let activity =
                job.payload.get("activity").cloned().ok_or_else(|| {
                    Error::Other("deliver_follow payload missing activity".into())
                })?;

            let target = Actor::fetch_by_id(pool, target_actor_id)
                .await?
                .ok_or_else(|| Error::Other("target actor not found".into()))?;
            let signer = Actor::fetch_by_id(pool, follower_actor_id)
                .await?
                .ok_or_else(|| Error::Other("follower actor not found".into()))?;
            let inbox = shared_inbox_or_inbox(&target);
            let inbox = Url::parse(&inbox)?;
            deliver_activity(pool, guard, &signer, &inbox, &activity).await
        }
        "deliver" => {
            let actor_id = job
                .payload
                .get("actor_id")
                .and_then(Value::as_i64)
                .ok_or_else(|| Error::Other("deliver payload missing actor_id".into()))?;
            let inbox_url = job
                .payload
                .get("inbox_url")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Other("deliver payload missing inbox_url".into()))?;
            let activity = job
                .payload
                .get("activity")
                .cloned()
                .ok_or_else(|| Error::Other("deliver payload missing activity".into()))?;

            let signer = Actor::fetch_by_id(pool, actor_id)
                .await?
                .ok_or_else(|| Error::Other("signer actor not found".into()))?;
            let inbox = Url::parse(inbox_url)?;
            deliver_activity(pool, guard, &signer, &inbox, &activity).await
        }
        other => Err(Error::Other(format!("unknown deliver kind: {other}"))),
    }
}

/// Shared inbox when advertised, else the personal inbox (ARCHITECTURE §4).
pub fn shared_inbox_or_inbox(actor: &Actor) -> String {
    actor
        .shared_inbox_url
        .clone()
        .unwrap_or_else(|| actor.inbox_url.clone())
}

/// Validate a public key PEM (used when refreshing a remote actor's key).
pub fn validate_public_key(pem: &str) -> bool {
    rsa::RsaPublicKey::from_public_key_pem(pem).is_ok()
}
