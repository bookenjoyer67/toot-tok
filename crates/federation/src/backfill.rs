//! Actor post backfill: after a successful follow (or on demand), fetch a
//! remote actor's recent public posts and ingest them through the same
//! `parse_inbound_note` → `ClipRow::create_remote` path the inbox Create
//! handler uses. Source is the actor's public Atom feed (Loops publishes one
//! at `/feeds/{id}.atom`; its `/v/{shortcode}` links resolve to full AP
//! Note objects). Idempotent: clips UNIQUE(ap_id) skips known objects.

use crate::egress::EgressGuard;
use crate::error::Error;
use crate::note::{parse_inbound_note, strip_html_tags};
use serde_json::Value;
use sqlx::PgPool;
use toottok_db::actor::Actor;
use toottok_db::clip::Clip as ClipRow;
use toottok_db::job::Job;
use url::Url;

/// Worker entry point for the `backfill_actor` job. Payload:
/// `{ "actor_id": <i64>, "max": <usize, optional> }`. Lives in the SERVER
/// worker (which owns retry bookkeeping); the crate only exposes the
/// enqueue helper and the core fetch+ingest.
pub fn process_backfill_job<'a>(
    pool: &'a PgPool,
    egress: &'a EgressGuard,
    job: Job,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        let actor_id = job.payload.get("actor_id").and_then(|v| v.as_i64());
        let max = job
            .payload
            .get("max")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;
        let Some(actor_id) = actor_id else {
            if let Err(e) =
                Job::dead_letter(pool, job.id, "malformed backfill_actor payload").await
            {
                tracing::error!(id = job.id, error = %e, "failed to dead-letter backfill job");
            }
            return;
        };

        match backfill_actor_posts(pool, egress, actor_id, max).await {
            Ok(created) => {
                tracing::info!(actor_id, created, "backfill complete");
                if let Err(e) = Job::mark_done(pool, job.id, None).await {
                    tracing::error!(id = job.id, error = %e, "failed to mark backfill done");
                }
            }
            Err(e) => {
                if let Err(de) = Job::dead_letter(
                    pool,
                    job.id,
                    &format!("backfill failed: {}", e.detail()),
                )
                .await
                {
                    tracing::error!(id = job.id, error = %de, "failed to dead-letter backfill");
                }
            }
        }
    })
}

/// Enqueue a backfill for an actor (no-op when one is already queued/done
/// recently — jobs are cheap and idempotent via clips UNIQUE(ap_id)).
pub async fn enqueue_backfill(pool: &PgPool, actor_id: i64, max: usize) -> Result<(), Error> {
    let payload = serde_json::json!({ "actor_id": actor_id, "max": max });
    Job::create(pool, "backfill_actor", &payload, None)
        .await
        .map(|_| ())
        .map_err(Error::from)
}

/// Backfill up to `max` recent public posts for a remote actor. Returns the
/// number of NEW clips created. Never errors on per-post failures — those are
/// logged and skipped so one bad object can't poison the run.
pub async fn backfill_actor_posts(
    pool: &PgPool,
    egress: &EgressGuard,
    actor_id: i64,
    max: usize,
) -> Result<usize, Error> {
    let actor = Actor::fetch_by_id(pool, actor_id)
        .await?
        .ok_or_else(|| Error::Other("backfill: actor not found".into()))?;
    if actor.domain.is_none() {
        return Err(Error::Other("backfill: actor is local".into()));
    }

    let feed_url = atom_feed_url(&actor)?;
    let Some(feed_url) = feed_url else {
        return Ok(0); // no discoverable public feed; nothing to do
    };

    let client = egress.client_for(&feed_url).await?;
    let resp = client.get(feed_url.as_str()).send().await?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!(
            "backfill: feed fetch {} failed with {}",
            feed_url,
            resp.status()
        )));
    }
    let body = resp.text().await?;
    let shortcodes = extract_video_links(&body);
    if shortcodes.is_empty() {
        return Ok(0);
    }

    let mut created = 0usize;
    for code in shortcodes.into_iter().take(max) {
        match fetch_and_ingest_one(pool, egress, &actor, &code).await {
            Ok(true) => created += 1,
            Ok(false) => {} // already known / skipped
            Err(e) => {
                tracing::warn!(actor = %actor.ap_id, shortcode = %code, error = %e, "backfill: post skipped");
            }
        }
    }
    Ok(created)
}

/// Loops pattern: AP id `https://host/ap/users/{n}` ↔ Atom feed
/// `https://host/feeds/{n}.atom`. Only https origins qualify.
fn atom_feed_url(actor: &Actor) -> Result<Option<Url>, Error> {
    let ap = Url::parse(&actor.ap_id)
        .map_err(|e| Error::Other(format!("backfill: bad ap_id: {e}")))?;
    if ap.scheme() != "https" {
        return Ok(None);
    }
    let host = ap.host_str().unwrap_or_default();
    let seg: Vec<&str> = ap.path_segments().map(|s| s.collect()).unwrap_or_default();
    // .../ap/users/{id}
    let id = seg.last().copied().unwrap_or_default();
    if !seg.contains(&"users") || id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return Ok(None);
    }
    Url::parse(&format!("https://{host}/feeds/{id}.atom"))
        .map(Some)
        .map_err(|e| Error::Other(format!("backfill: bad feed url: {e}")))
}

/// Pull `/v/{shortcode}` links out of an Atom document.
fn extract_video_links(atom_xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = atom_xml;
    while let Some(pos) = rest.find("/v/") {
        let tail = &rest[pos + 3..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '~'))
            .unwrap_or(tail.len());
        if end > 0 {
            let code = &tail[..end];
            if !out.iter().any(|s: &String| s == code) {
                out.push(code.to_string());
            }
        }
        rest = &tail[end.min(tail.len())..];
    }
    out
}

/// Resolve one `/v/{code}` to its AP Note and ingest it as a clip.
/// Returns `Ok(false)` when the object was already known or failed
/// validation (no mp4 attachment etc).
async fn fetch_and_ingest_one(
    pool: &PgPool,
    egress: &EgressGuard,
    actor: &Actor,
    code: &str,
) -> Result<bool, Error> {
    let page = format!("https://loops.video/v/{code}");
    let url =
        Url::parse(&page).map_err(|e| Error::Other(format!("backfill: bad shortcode url: {e}")))?;
    let client = egress.client_for(&url).await?;
    let resp = client
        .get(url.as_str())
        .header("accept", "application/activity+json")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Err(Error::Other(format!("note fetch {}", resp.status())));
    }
    let note: Value = resp.json().await?;

    let parsed = match parse_inbound_note(&note) {
        Ok(p) => p,
        Err(reason) => {
            tracing::debug!(%code, %reason, "backfill: not an ingestible video note");
            return Ok(false);
        }
    };

    // The fetched object must really belong to this actor (anti-spoof, same
    // rule as the inbox handler).
    if parsed.attributed_to != actor.ap_id {
        return Err(Error::Other(
            "backfill: note attributedTo does not match actor".into(),
        ));
    }

    let caption = parsed
        .content_html
        .as_deref()
        .map(strip_html_tags)
        .filter(|s| !s.is_empty());

    match ClipRow::create_remote(
        pool,
        actor.id,
        &parsed.id,
        caption.as_deref(),
        parsed.attachment.duration_s,
        parsed.attachment.width,
        parsed.attachment.height,
        &parsed.attachment.media_url,
        parsed.sensitive,
        parsed.summary.as_deref(),
        parsed.published,
    )
    .await
    {
        Ok(_) => Ok(true),
        Err(e) if e.is_unique_violation() => Ok(false),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::extract_video_links;

    #[test]
    fn extracts_unique_shortcodes_from_atom() {
        let xml = r#"<feed>
<entry><link href="https://loops.video/v/hAzyPuzhLH"/></entry>
<entry><link href="https://loops.video/v/hAzyPuzhLH"/></entry>
<entry><link href="https://loops.video/v/xY9_-z"/></entry>
</feed>"#;
        assert_eq!(extract_video_links(xml), vec!["hAzyPuzhLH", "xY9_-z"]);
    }

    #[test]
    fn empty_feed_yields_nothing() {
        assert!(extract_video_links("<feed></feed>").is_empty());
    }
}
