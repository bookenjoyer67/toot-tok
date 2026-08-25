//! ActivityPub activity types and the inbound processing pipeline.
//!
//! Inbound pipeline STRICT ORDER (ARCHITECTURE.md §4): HTTP signature
//! verification (done by the crate's `receive_activity`), activity_id
//! idempotency gate (`activities.activity_id` UNIQUE — insert-first with
//! `ON CONFLICT DO NOTHING`, skip when no row was inserted), tombstone check,
//! store raw JSONB, process, then stamp `processed_at`.
//!
//! Unknown activity types are captured by [`Passthrough`]: stored raw, never
//! processed, answered 202.

use std::str::FromStr;

use activitypub_federation::config::Data;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::kinds::activity::{
    AcceptType, CreateType, DeleteType, FollowType, MoveType, RejectType, UndoType, UpdateType,
};
use activitypub_federation::protocol::tombstone::Tombstone;
use activitypub_federation::traits::{Activity, Actor, Object};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use toottok_db::activity::Activity as ActivityRow;
use toottok_db::clip::Clip as ClipRow;
use toottok_db::follow::Follow as FollowRow;
use toottok_db::tombstone::Tombstone as TombstoneRow;
use tracing::{info, warn};
use url::Url;
use uuid::Uuid;

use crate::data::FederationData;
use crate::deliver::enqueue_delivery;
use crate::error::Error;
use crate::note;
use crate::note::strip_html_tags;
use crate::object::{actor_type_from_json, DbActor, RemoteActorParts};

/// `Follow` — a remote actor requesting to follow a local one (or our outbound
/// follow being delivered). The `object` is the target actor.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Follow {
    #[serde(rename = "type")]
    pub kind: FollowType,
    pub id: Url,
    pub actor: ObjectId<DbActor>,
    pub object: ObjectId<DbActor>,
}

/// The `object` of `Accept`/`Reject`/`Undo`: either the full embedded `Follow`
/// activity (Mastodon-style), a minimal `{id, type: Follow}` reference, or just
/// the Follow activity URL.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FollowObject {
    Follow(Follow),
    Ref(FollowRef),
    Url(Url),
}

/// Minimal reference form of a Follow activity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowRef {
    #[serde(rename = "type")]
    pub kind: FollowType,
    pub id: Url,
}

impl FollowObject {
    pub fn id(&self) -> &Url {
        match self {
            FollowObject::Follow(f) => &f.id,
            FollowObject::Ref(r) => &r.id,
            FollowObject::Url(u) => u,
        }
    }
}

/// `Accept(Follow)` — confirms an outbound follow.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accept {
    #[serde(rename = "type")]
    pub kind: AcceptType,
    pub id: Url,
    pub actor: ObjectId<DbActor>,
    pub object: FollowObject,
}

/// `Reject(Follow)`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reject {
    #[serde(rename = "type")]
    pub kind: RejectType,
    pub id: Url,
    pub actor: ObjectId<DbActor>,
    pub object: FollowObject,
}

/// `Undo` — lenient by design: `object` stays raw JSON so an
/// `Undo(Like)`/`Undo(Announce)` (Mastodon unlikes/reblogs) can never fail
/// the inbox POST with a 400. Follow-shaped objects are interpreted and
/// processed; everything else is stored unprocessed and answered 202
/// (F1: a Mastodon unlike must never permanently fail).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Undo {
    #[serde(rename = "type")]
    pub kind: UndoType,
    pub id: Url,
    pub actor: ObjectId<DbActor>,
    pub object: Value,
}

/// Interpret a raw `Undo.object`: the id of the undone activity when it is
/// (or references) a `Follow`, `None` for every other shape (`Like`,
/// `Announce`, arrays without a Follow entry, …).
fn undo_follow_target(object: &Value) -> Option<String> {
    match object {
        // Bare URL string: assume the sender undoes a Follow.
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => {
            let kind = o.get("type").and_then(Value::as_str).unwrap_or("");
            if kind != "Follow" {
                return None;
            }
            o.get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        }
        // Mastodon-style array: first Follow-shaped entry wins.
        Value::Array(items) => items.iter().find_map(undo_follow_target),
        _ => None,
    }
}

/// `Delete` — object is either a [`Tombstone`] or the deleted object's URL.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DeleteObject {
    Tombstone(Tombstone),
    Url(Url),
}

impl DeleteObject {
    pub fn id(&self) -> &Url {
        match self {
            DeleteObject::Tombstone(t) => &t.id,
            DeleteObject::Url(u) => u,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delete {
    #[serde(rename = "type")]
    pub kind: DeleteType,
    pub id: Url,
    pub actor: ObjectId<DbActor>,
    pub object: DeleteObject,
}

/// `Create` — an object being published. Wave B processes embedded
/// `Note`s with a video attachment into remote clip rows; every other
/// embedded shape is stored unprocessed (202) by the lenient fallback.
/// The object is kept as raw JSON so malformed payloads can never fail the
/// whole inbox POST with a 400.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Create {
    #[serde(rename = "type")]
    pub kind: CreateType,
    pub id: Url,
    pub actor: ObjectId<DbActor>,
    pub object: Value,
}

/// `Update` — minimal wave-B semantics: refresh caption/sensitivity/CW on a
/// matching clip row when the sender owns it; logged and ignored otherwise.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    #[serde(rename = "type")]
    pub kind: UpdateType,
    pub id: Url,
    pub actor: ObjectId<DbActor>,
    pub object: Value,
}

/// `Move` — account migration. Wave A logs only.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Move {
    #[serde(rename = "type")]
    pub kind: MoveType,
    pub id: Url,
    pub actor: ObjectId<DbActor>,
    pub object: ObjectId<DbActor>,
    #[serde(default)]
    pub target: Option<Url>,
}

/// Catch-all for activity types we don't process yet (Like/Announce/…):
/// the raw JSON is stored for the activity log and the request answers 202.
#[derive(Clone, Debug)]
pub struct Passthrough {
    pub id: Url,
    pub actor: ObjectId<DbActor>,
    pub raw: Value,
}

impl<'de> Deserialize<'de> for Passthrough {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .and_then(|s| Url::parse(s).ok())
            .ok_or_else(|| serde::de::Error::custom("passthrough activity has no id"))?;
        let actor = value
            .get("actor")
            .and_then(Value::as_str)
            .and_then(|s| Url::parse(s).ok())
            .map(ObjectId::<DbActor>::from)
            .ok_or_else(|| serde::de::Error::custom("passthrough activity has no actor"))?;
        Ok(Passthrough {
            id,
            actor,
            raw: value,
        })
    }
}

impl Serialize for Passthrough {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.raw.serialize(serializer)
    }
}

/// Every activity type the inbox will process, plus the catch-all.
#[derive(Clone, Debug)]
pub enum ApActivities {
    Follow(Follow),
    Accept(Accept),
    Reject(Reject),
    Undo(Undo),
    Delete(Delete),
    Create(Create),
    Update(Update),
    Move(Move),
    Passthrough(Passthrough),
}

// `#[serde(untagged)]` requires manual Deserialize so the catch-all is tried
// LAST (and only after inspecting the `type` discriminator). `Create` /
// `Update` degrade to [`Passthrough`] when their embedded object cannot be
// captured — a malformed payload must be STORED + 202, never a 400.
impl<'de> Deserialize<'de> for ApActivities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let kind = value.get("type").and_then(Value::as_str).unwrap_or("");
        let result = match kind {
            "Follow" => Follow::deserialize(value).map(ApActivities::Follow),
            "Accept" => Accept::deserialize(value).map(ApActivities::Accept),
            "Reject" => Reject::deserialize(value).map(ApActivities::Reject),
            "Undo" => match Undo::deserialize(value.clone()) {
                Ok(undo) => Ok(ApActivities::Undo(undo)),
                // Malformed Undo (no id/actor): stored unprocessed, 202 —
                // never a 400.
                Err(_) => Passthrough::deserialize(value).map(ApActivities::Passthrough),
            },
            "Delete" => Delete::deserialize(value).map(ApActivities::Delete),
            "Create" => match Create::deserialize(value.clone()) {
                Ok(create) => Ok(ApActivities::Create(create)),
                Err(_) => Passthrough::deserialize(value).map(ApActivities::Passthrough),
            },
            "Update" => match Update::deserialize(value.clone()) {
                Ok(update) => Ok(ApActivities::Update(update)),
                Err(_) => Passthrough::deserialize(value).map(ApActivities::Passthrough),
            },
            "Move" => Move::deserialize(value).map(ApActivities::Move),
            _ => Passthrough::deserialize(value).map(ApActivities::Passthrough),
        };
        result.map_err(serde::de::Error::custom)
    }
}

impl Serialize for ApActivities {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ApActivities::Follow(a) => a.serialize(serializer),
            ApActivities::Accept(a) => a.serialize(serializer),
            ApActivities::Reject(a) => a.serialize(serializer),
            ApActivities::Undo(a) => a.serialize(serializer),
            ApActivities::Delete(a) => a.serialize(serializer),
            ApActivities::Create(a) => a.serialize(serializer),
            ApActivities::Update(a) => a.serialize(serializer),
            ApActivities::Move(a) => a.serialize(serializer),
            ApActivities::Passthrough(a) => a.serialize(serializer),
        }
    }
}

#[async_trait]
impl Activity for ApActivities {
    type DataType = FederationData;
    type Error = Error;

    fn id(&self) -> &Url {
        match self {
            ApActivities::Follow(a) => &a.id,
            ApActivities::Accept(a) => &a.id,
            ApActivities::Reject(a) => &a.id,
            ApActivities::Undo(a) => &a.id,
            ApActivities::Delete(a) => &a.id,
            ApActivities::Create(a) => &a.id,
            ApActivities::Update(a) => &a.id,
            ApActivities::Move(a) => &a.id,
            ApActivities::Passthrough(a) => &a.id,
        }
    }

    fn actor(&self) -> &Url {
        match self {
            ApActivities::Follow(a) => a.actor.inner(),
            ApActivities::Accept(a) => a.actor.inner(),
            ApActivities::Reject(a) => a.actor.inner(),
            ApActivities::Undo(a) => a.actor.inner(),
            ApActivities::Delete(a) => a.actor.inner(),
            ApActivities::Create(a) => a.actor.inner(),
            ApActivities::Update(a) => a.actor.inner(),
            ApActivities::Move(a) => a.actor.inner(),
            ApActivities::Passthrough(a) => a.actor.inner(),
        }
    }

    async fn verify(&self, _data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn receive(self, data: &Data<Self::DataType>) -> Result<(), Self::Error> {
        match self {
            ApActivities::Follow(a) => a.receive_inbound(data).await,
            ApActivities::Accept(a) => a.receive_inbound(data).await,
            ApActivities::Reject(a) => a.receive_inbound(data).await,
            ApActivities::Undo(a) => a.receive_inbound(data).await,
            ApActivities::Delete(a) => a.receive_inbound(data).await,
            ApActivities::Create(a) => a.receive_inbound(data).await,
            ApActivities::Update(a) => a.receive_inbound(data).await,
            ApActivities::Move(a) => a.receive_inbound(data).await,
            ApActivities::Passthrough(a) => a.receive_inbound(data).await,
        }
    }
}

/// ── inbound pipeline primitives ──────────────────────────────────────────────
/// Idempotency gate (step 2). Returns `Ok(false)` when the activity was already
/// received (duplicate delivery → caller skips). On first sight the raw JSON is
/// stored here (step 4) as part of the same insert.
async fn inbound_begin(
    pool: &PgPool,
    activity_id: &str,
    actor_ap_id: &str,
    object_ap_id: Option<&str>,
    raw: &Value,
) -> Result<bool, Error> {
    let inserted =
        ActivityRow::try_create_inbound(pool, activity_id, actor_ap_id, object_ap_id, raw).await?;
    Ok(inserted.is_some())
}

/// Stamp `processed_at` (step 6) after a successful pipeline run.
async fn inbound_finish(pool: &PgPool, activity_id: &str) -> Result<(), Error> {
    ActivityRow::stamp_processed(pool, activity_id).await?;
    Ok(())
}

/// Tombstone check (step 3): when the object of this activity is already
/// tombstoned, swallow it silently (delete-wins; later Creates of a deleted
/// object are ignored).
async fn tombstoned(pool: &PgPool, object_ap_id: &str) -> Result<bool, Error> {
    Ok(TombstoneRow::exists(pool, object_ap_id).await?)
}

/// Pull a best-effort `object` id out of an arbitrary activity's raw JSON.
fn raw_object_id(raw: &Value) -> Option<String> {
    match raw.get("object") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Object(o)) => o.get("id").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

/// The id of an EMBEDDED object (`Create`/`Update` carry the Note itself).
fn embedded_object_id(object: &Value) -> Option<String> {
    object
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Replicates the crate's `is_local_url` (its `config` field is crate-private):
/// a URL is local when `host[:port]` equals the configured federation domain.
pub fn is_local_url(url: &Url, domain: &str) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let with_port = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    with_port == domain
}

/// A new activity id on this instance's domain.
fn activity_id(base_url: &str) -> Url {
    Url::parse(&format!("{base_url}/activities/{}", Uuid::new_v4()))
        .expect("activity id url parses")
}

/// ── per-type processing ──────────────────────────────────────────────────────
impl Follow {
    async fn receive_inbound(self, data: &Data<FederationData>) -> Result<(), Error> {
        let pool = &data.pool;
        let actor_ap_id = self.actor.inner().to_string();
        let object_ap_id = self.object.inner().to_string();
        let raw = serde_json::to_value(&self)?;

        if !inbound_begin(
            pool,
            self.id.as_str(),
            &actor_ap_id,
            Some(&object_ap_id),
            &raw,
        )
        .await?
        {
            return Ok(()); // duplicate delivery
        }
        if tombstoned(pool, &object_ap_id).await? {
            inbound_finish(pool, self.id.as_str()).await?;
            return Ok(()); // target deleted: swallow silently
        }

        let target = self.object.dereference_local(data).await?;
        if !is_local_url(target.id(), &data.domain) {
            // Remote-follows-remote is not ours to record.
            inbound_finish(pool, self.id.as_str()).await?;
            return Ok(());
        }
        let follower = self.actor.dereference_local(data).await?;

        FollowRow::upsert(
            pool,
            follower.row.id,
            target.row.id,
            Some(self.id.as_str()),
            "requested",
        )
        .await?;

        // Auto-accept unless the target is locked (manual approval).
        if !target.row.is_locked {
            FollowRow::set_state(pool, follower.row.id, target.row.id, "accepted").await?;
            let accept_json = accept_activity(
                &data.base_url,
                &target.ap_id,
                self.id.as_str(),
                &actor_ap_id,
                &object_ap_id,
            );
            let accept_id = accept_json
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let _ = ActivityRow::create_outbound(
                pool,
                &accept_id,
                target.ap_id.as_str(),
                Some(self.id.as_str()),
                &accept_json,
            )
            .await;
            let inbox = follower.shared_inbox_or_inbox();
            enqueue_delivery(pool, target.row.id, inbox.as_str(), &accept_json).await?;
        }

        inbound_finish(pool, self.id.as_str()).await?;
        Ok(())
    }
}

/// Build the `Accept(Follow)` activity JSON (embedded Follow object) signed and
/// delivered by the worker.
pub fn accept_activity(
    base_url: &str,
    actor: &Url,
    follow_id: &str,
    follow_actor: &str,
    follow_object: &str,
) -> Value {
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("{base_url}/activities/{}", Uuid::new_v4()),
        "type": "Accept",
        "actor": actor.as_str(),
        "object": {
            "id": follow_id,
            "type": "Follow",
            "actor": follow_actor,
            "object": follow_object,
        },
        "to": [follow_actor],
    })
}

impl Accept {
    async fn receive_inbound(self, data: &Data<FederationData>) -> Result<(), Error> {
        let pool = &data.pool;
        let actor_ap_id = self.actor.inner().to_string();
        let follow_id = self.object.id().to_string();
        let raw = serde_json::to_value(&self)?;

        if !inbound_begin(pool, self.id.as_str(), &actor_ap_id, Some(&follow_id), &raw).await? {
            return Ok(());
        }

        // The accepted follow: our outbound follow confirmed → state=accepted.
        if let Some(follow) = FollowRow::fetch_by_activity_id(pool, &follow_id).await? {
            FollowRow::set_state(
                pool,
                follow.follower_actor_id,
                follow.target_actor_id,
                "accepted",
            )
            .await?;
            // Real-fediverse UX: a fresh remote connection should show
            // content, not an empty profile. Queue a best-effort backfill of
            // the actor's recent public posts (Atom → AP objects → same
            // ingest path as inbox Creates). Failures are logged by the
            // worker; they must never fail the Accept.
            if let Err(e) =
                crate::backfill::enqueue_backfill(pool, follow.target_actor_id, 20).await
            {
                tracing::warn!(error = %e, "backfill enqueue failed; continuing");
            }
        }
        inbound_finish(pool, self.id.as_str()).await?;
        Ok(())
    }
}

impl Reject {
    async fn receive_inbound(self, data: &Data<FederationData>) -> Result<(), Error> {
        let pool = &data.pool;
        let actor_ap_id = self.actor.inner().to_string();
        let follow_id = self.object.id().to_string();
        let raw = serde_json::to_value(&self)?;

        if !inbound_begin(pool, self.id.as_str(), &actor_ap_id, Some(&follow_id), &raw).await? {
            return Ok(());
        }
        if let Some(follow) = FollowRow::fetch_by_activity_id(pool, &follow_id).await? {
            FollowRow::set_state(
                pool,
                follow.follower_actor_id,
                follow.target_actor_id,
                "rejected",
            )
            .await?;
        }
        inbound_finish(pool, self.id.as_str()).await?;
        Ok(())
    }
}

impl Undo {
    async fn receive_inbound(self, data: &Data<FederationData>) -> Result<(), Error> {
        let pool = &data.pool;
        let actor_ap_id = self.actor.inner().to_string();
        let raw = serde_json::to_value(&self)?;

        // Lenient dispatch: only a Follow-shaped object drives state. An
        // Undo(Like)/Undo(Announce) (or any other shape) is stored
        // unprocessed and answered 202 — Mastodon unlikes MUST NOT 400.
        let Some(follow_id) = undo_follow_target(&self.object) else {
            if !inbound_begin(
                pool,
                self.id.as_str(),
                &actor_ap_id,
                raw_object_id(&raw).as_deref(),
                &raw,
            )
            .await?
            {
                return Ok(()); // duplicate delivery
            }
            info!(
                activity_id = %self.id,
                actor = %actor_ap_id,
                object_type = self
                    .object
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or(""),
                "Undo of non-Follow object; stored unprocessed"
            );
            inbound_finish(pool, self.id.as_str()).await?;
            return Ok(());
        };

        if !inbound_begin(pool, self.id.as_str(), &actor_ap_id, Some(&follow_id), &raw).await? {
            return Ok(());
        }
        if let Some(follow) = FollowRow::fetch_by_activity_id(pool, &follow_id).await? {
            FollowRow::delete(pool, follow.follower_actor_id, follow.target_actor_id).await?;
        }
        inbound_finish(pool, self.id.as_str()).await?;
        Ok(())
    }
}

impl Delete {
    async fn receive_inbound(self, data: &Data<FederationData>) -> Result<(), Error> {
        let pool = &data.pool;
        let actor_ap_id = self.actor.inner().to_string();
        let object_id = self.object.id().to_string();
        let raw = serde_json::to_value(&self)?;

        if !inbound_begin(pool, self.id.as_str(), &actor_ap_id, Some(&object_id), &raw).await? {
            return Ok(());
        }

        // Clip tombstone (wave B): a Delete whose object is a clip ap_id we
        // know flips the cached row — this covers remote-originated deletes
        // AND our own propagated deletes arriving back. Only the clip's
        // author may delete it; anyone else's Delete is logged and ignored.
        if let Some(clip) = ClipRow::fetch_by_ap_id(pool, &object_id).await? {
            let owner = toottok_db::actor::Actor::fetch_by_id(pool, clip.actor_id)
                .await?
                .map(|a| a.ap_id == actor_ap_id)
                .unwrap_or(false);
            if owner {
                TombstoneRow::upsert(pool, &object_id, "Note").await?;
                ClipRow::mark_deleted(pool, clip.id).await?;
                info!(clip_id = clip.id, ap_id = %object_id, "clip deleted by federated Delete");
            } else {
                warn!(
                    activity_id = %self.id,
                    actor = %actor_ap_id,
                    ap_id = %object_id,
                    "Delete for a clip by a non-owner actor; ignored"
                );
            }
            inbound_finish(pool, self.id.as_str()).await?;
            return Ok(());
        }

        // Tombstone wins: record it, then mark any cached actor row deleted.
        TombstoneRow::upsert(pool, &object_id, "Person").await?;
        let _ = toottok_db::actor::Actor::mark_remote_deleted_by_ap_id(pool, &object_id).await;
        inbound_finish(pool, self.id.as_str()).await?;
        Ok(())
    }
}

impl Create {
    async fn receive_inbound(self, data: &Data<FederationData>) -> Result<(), Error> {
        let pool = &data.pool;
        let actor_ap_id = self.actor.inner().to_string();
        let object_id = embedded_object_id(&self.object);
        let raw = serde_json::to_value(&self)?;

        if !inbound_begin(
            pool,
            self.id.as_str(),
            &actor_ap_id,
            object_id.as_deref(),
            &raw,
        )
        .await?
        {
            return Ok(()); // duplicate delivery: idempotent skip
        }

        let Some(object_id) = object_id else {
            info!(activity_id = %self.id, "Create without an object id; stored unprocessed");
            inbound_finish(pool, self.id.as_str()).await?;
            return Ok(());
        };

        // Tombstone wins over a later Create of the same object.
        if tombstoned(pool, &object_id).await? {
            inbound_finish(pool, self.id.as_str()).await?;
            return Ok(());
        }

        // Loops media rules: Note + Document|Video mp4 attachment. Anything
        // else is stored (and answered 202) but never becomes a clip.
        let parsed = match note::parse_inbound_note(&self.object) {
            Ok(p) => p,
            Err(reason) => {
                info!(
                    activity_id = %self.id,
                    ap_id = %object_id,
                    %reason,
                    "Create(Note) failed media validation; stored without creating a clip"
                );
                inbound_finish(pool, self.id.as_str()).await?;
                return Ok(());
            }
        };

        // attributedTo MUST be a remote actor, and must be the signed sender:
        // a local target would be self-addressed, a mismatched one spoofed.
        let Ok(attributed) = Url::parse(&parsed.attributed_to) else {
            info!(activity_id = %self.id, "Create with unparseable attributedTo; ignored");
            inbound_finish(pool, self.id.as_str()).await?;
            return Ok(());
        };
        if is_local_url(&attributed, &data.domain) || attributed.as_str() != actor_ap_id {
            warn!(
                activity_id = %self.id,
                actor = %actor_ap_id,
                attributed = %parsed.attributed_to,
                "Create attributedTo is not the remote signing actor; ignored"
            );
            inbound_finish(pool, self.id.as_str()).await?;
            return Ok(());
        }

        // Resolve the author row, lazily fetching unknown actors through the
        // crate's egress-guarded client.
        let author = match self.actor.dereference(data).await {
            Ok(a) => a,
            Err(e) => {
                warn!(
                    activity_id = %self.id,
                    actor = %actor_ap_id,
                    error = %e,
                    "Create actor could not be resolved; stored unprocessed"
                );
                inbound_finish(pool, self.id.as_str()).await?;
                return Ok(());
            }
        };

        // Remote captions are sanitized by stripping ALL tags (v1 stance).
        let caption = parsed
            .content_html
            .as_deref()
            .map(note::strip_html_tags)
            .filter(|s| !s.is_empty());

        match ClipRow::create_remote(
            pool,
            author.row.id,
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
            Ok(clip) => {
                info!(
                    clip_id = clip.id,
                    ap_id = %clip.ap_id,
                    author = %actor_ap_id,
                    "remote clip created from Create(Note)"
                );
            }
            // Same object delivered under a different activity id: the clips
            // UNIQUE(ap_id) gate keeps us idempotent there too.
            Err(e) if e.is_unique_violation() => {
                info!(ap_id = %parsed.id, "remote clip already known; skipping");
            }
            Err(e) => return Err(e.into()),
        }

        inbound_finish(pool, self.id.as_str()).await?;
        Ok(())
    }
}

impl Update {
    async fn receive_inbound(self, data: &Data<FederationData>) -> Result<(), Error> {
        let pool = &data.pool;
        let actor_ap_id = self.actor.inner().to_string();
        let object_id = embedded_object_id(&self.object);
        let raw = serde_json::to_value(&self)?;

        if !inbound_begin(
            pool,
            self.id.as_str(),
            &actor_ap_id,
            object_id.as_deref(),
            &raw,
        )
        .await?
        {
            return Ok(()); // duplicate delivery
        }

        // Minimal wave-B semantics: when the object is a clip we know AND the
        // sender is its owner, refresh caption/sensitivity/CW; otherwise log
        // and ignore. Every Update is logged.
        info!(
            activity_id = %self.id,
            actor = %actor_ap_id,
            object = ?object_id,
            "Update received"
        );

        if let Some(object_id) = object_id.as_deref() {
            if let Some(clip) = ClipRow::fetch_by_ap_id(pool, object_id).await? {
                let owner = toottok_db::actor::Actor::fetch_by_id(pool, clip.actor_id)
                    .await?
                    .map(|a| a.ap_id == actor_ap_id)
                    .unwrap_or(false);
                let fields = note::extract_note_fields(&self.object);
                if owner && clip.deleted_at.is_none() {
                    let caption = fields
                        .content_html
                        .map(|c| note::strip_html_tags(&c))
                        .filter(|s| !s.is_empty());
                    ClipRow::update_note_fields(
                        pool,
                        clip.id,
                        caption.as_deref(),
                        fields.sensitive,
                        fields.summary.as_deref(),
                    )
                    .await?;
                    info!(clip_id = clip.id, "clip updated from Update(Note)");
                } else {
                    info!(
                        clip_id = clip.id,
                        owner,
                        deleted = clip.deleted_at.is_some(),
                        "Update ignored (not owner or clip deleted)"
                    );
                }
            }
        }

        inbound_finish(pool, self.id.as_str()).await?;
        Ok(())
    }
}

impl Move {
    async fn receive_inbound(self, data: &Data<FederationData>) -> Result<(), Error> {
        let pool = &data.pool;
        let actor_ap_id = self.actor.inner().to_string();
        let object_id = self.object.inner().to_string();
        let raw = serde_json::to_value(&self)?;

        if !inbound_begin(pool, self.id.as_str(), &actor_ap_id, Some(&object_id), &raw).await? {
            return Ok(());
        }
        // Wave A: log-and-accept only.
        warn!(
            activity_id = %self.id,
            actor = %self.actor.inner(),
            target = ?self.target,
            "Move activity received; account migration not implemented (logged only)"
        );
        inbound_finish(pool, self.id.as_str()).await?;
        Ok(())
    }
}

impl Passthrough {
    async fn receive_inbound(self, data: &Data<FederationData>) -> Result<(), Error> {
        let pool = &data.pool;
        let actor_ap_id = self.actor.inner().to_string();
        let object_id = raw_object_id(&self.raw);

        if !inbound_begin(
            pool,
            self.id.as_str(),
            &actor_ap_id,
            object_id.as_deref(),
            &self.raw,
        )
        .await?
        {
            return Ok(());
        }
        // Unknown type: stored, not processed, answered 202.
        inbound_finish(pool, self.id.as_str()).await?;
        Ok(())
    }
}

/// Outbound activity builders shared by the API and worker paths.
/// `Follow` activity JSON for our outbound follow.
pub fn follow_activity(base_url: &str, follower_ap_id: &str, target_ap_id: &str) -> Value {
    let id = activity_id(base_url);
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": id.as_str(),
        "type": "Follow",
        "actor": follower_ap_id,
        "object": target_ap_id,
        "to": [target_ap_id],
        "cc": [format!("{follower_ap_id}/followers")],
    })
}

/// `Undo(Follow)` activity JSON for our outbound unfollow.
pub fn undo_activity(base_url: &str, actor_ap_id: &str, follow_id: &str) -> Value {
    let id = activity_id(base_url);
    json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": id.as_str(),
        "type": "Undo",
        "actor": actor_ap_id,
        "object": { "id": follow_id, "type": "Follow" },
        "to": [format!("{actor_ap_id}/followers")],
    })
}

/// Extract the `id` (as string) from a built activity JSON.
pub fn activity_id_from_json(activity: &Value) -> String {
    activity
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Parse the actor JSON from a remote fetch into the parts needed for the
/// `actors` row.
pub fn parse_remote_actor_json(value: &Value) -> Result<RemoteActorParts, Error> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Other("remote actor json missing id".into()))?;
    let id = Url::from_str(id)?;
    let domain = id
        .host_str()
        .ok_or_else(|| Error::Other("remote actor id has no host".into()))?
        .to_string();
    let username = value
        .get("preferredUsername")
        .or_else(|| value.get("preferred_username"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let actor_type = actor_type_from_json(value).as_db().to_string();
    let inbox = value
        .get("inbox")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Other("remote actor json missing inbox".into()))?
        .to_string();
    let outbox = value
        .get("outbox")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string());
    let followers = value
        .get("followers")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string());
    let public_key_pem = value
        .get("publicKey")
        .and_then(|k| k.get("publicKeyPem").or_else(|| k.get("public_key_pem")))
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Other("remote actor json missing publicKey".into()))?
        .to_string();
    // Profile fields: `name` is the display name, `summary` the (HTML)
    // bio — strip tags so it renders as plain text — and `icon.url` the
    // remote avatar (hot-linked; never downloaded).
    let display_name = value
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let summary = value
        .get("summary")
        .and_then(Value::as_str)
        .map(strip_html_tags)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let avatar_url = value
        .get("icon")
        .and_then(|icon| icon.get("url"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"));
    Ok(RemoteActorParts {
        id: id.to_string(),
        domain,
        username,
        actor_type,
        public_key_pem,
        inbox,
        shared_inbox: value
            .get("endpoints")
            .and_then(|e| e.get("sharedInbox").or_else(|| e.get("shared_inbox")))
            .or_else(|| value.get("sharedInbox"))
            .and_then(Value::as_str)
            .map(str::to_string),
        outbox,
        followers,
        display_name,
        summary,
        avatar_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_delete_with_embedded_tombstone() {
        let raw = json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": "http://localhost:1/activities/abc",
            "type": "Delete",
            "actor": "http://localhost:2/users/bob",
            "object": { "id": "http://localhost:2/clips/7", "type": "Tombstone" },
            "to": ["http://localhost:1/users/alice"],
        });
        match ApActivities::deserialize(&raw).expect("delete parses") {
            ApActivities::Delete(d) => {
                assert_eq!(d.object.id().as_str(), "http://localhost:2/clips/7");
            }
            other => panic!("expected Delete variant, got {other:?}"),
        }
    }

    #[test]
    fn create_falls_back_to_passthrough_when_object_missing() {
        let raw = json!({
            "id": "http://localhost:1/activities/x",
            "type": "Create",
            "actor": "http://localhost:2/users/bob",
        });
        let parsed = ApActivities::deserialize(&raw).expect("still parses");
        assert!(
            !matches!(parsed, ApActivities::Create(_)),
            "create must not parse without an object"
        );
    }

    #[test]
    fn parses_create_note_and_update() {
        let note = json!({
            "id": "http://localhost:2/clips/7",
            "type": "Note",
            "attributedTo": "http://localhost:2/users/bob",
            "content": "hi",
            "attachment": [{ "type": "Document", "mediaType": "video/mp4", "url": "http://x/v.mp4" }],
        });
        let raw = json!({
            "id": "http://localhost:1/activities/y",
            "type": "Create",
            "actor": "http://localhost:2/users/bob",
            "object": note,
        });
        assert!(matches!(
            ApActivities::deserialize(&raw).expect("create parses"),
            ApActivities::Create(_)
        ));

        let upd = json!({
            "id": "http://localhost:1/activities/z",
            "type": "Update",
            "actor": "http://localhost:2/users/bob",
            "object": { "id": "http://localhost:2/clips/7", "type": "Note" },
        });
        assert!(matches!(
            ApActivities::deserialize(&upd).expect("update parses"),
            ApActivities::Update(_)
        ));
    }

    #[test]
    fn undo_of_like_parses_leniently_instead_of_400() {
        // F1: Mastodon sends Undo(Like) for unlikes; the raw object must
        // parse (no 400) and must NOT be interpreted as a Follow.
        let raw = json!({
            "@context": "https://www.w3.org/ns/activitystreams",
            "id": "https://b.test/activities/undo-like-1",
            "type": "Undo",
            "actor": "https://a.test/users/alice",
            "object": {
                "id": "https://a.test/activities/like-1",
                "type": "Like",
                "actor": "https://a.test/users/alice",
                "object": "https://b.test/clips/9",
            },
        });
        match ApActivities::deserialize(&raw).expect("Undo(Like) parses") {
            ApActivities::Undo(undo) => {
                assert_eq!(
                    undo_follow_target(&undo.object),
                    None,
                    "Undo(Like) is not a follow undo"
                );
                assert_eq!(
                    undo.object["type"], "Like",
                    "raw object preserved for the activity log"
                );
            }
            other => panic!("expected Undo variant, got {other:?}"),
        }
    }

    #[test]
    fn undo_follow_shapes_still_interpret() {
        let embedded = json!({
            "id": "https://a.test/follows/1",
            "type": "Follow",
            "actor": "https://a.test/users/alice",
            "object": "https://b.test/users/bob",
        });
        assert_eq!(
            undo_follow_target(&embedded).as_deref(),
            Some("https://a.test/follows/1")
        );
        let bare = json!("https://a.test/follows/2");
        assert_eq!(
            undo_follow_target(&bare).as_deref(),
            Some("https://a.test/follows/2")
        );
        let minimal = json!({ "id": "https://a.test/follows/3", "type": "Follow" });
        assert_eq!(
            undo_follow_target(&minimal).as_deref(),
            Some("https://a.test/follows/3")
        );
        let announce = json!({ "id": "https://a.test/announces/1", "type": "Announce" });
        assert_eq!(undo_follow_target(&announce), None);
    }

    #[test]
    fn malformed_undo_without_ids_fails_parse() {
        // F1 design: 400 is reserved for bodies with no usable id at all —
        // they cannot pass the idempotency gate, so they must NOT parse into
        // a Passthrough (which would require an id). Truly malformed = parse
        // error; lenient Undo(Like) WITH ids degrades to passthrough + 202.
        let raw = json!({
            "type": "Undo",
            "id": "https://a.test/activities/undo-like-1",
            "actor": "https://a.test/users/alice",
            "object": { "id": "https://a.test/activities/like-1", "type": "Like" },
        });
        let parsed = ApActivities::deserialize(&raw);
        assert!(
            matches!(parsed, Ok(ApActivities::Undo(_))),
            "undo of Like parses as LENIENT Undo (raw object) — processed \
             as unprocessed-store + 202, never a 400"
        );
        let idless = json!({
            "type": "Undo",
            "actor": "https://a.test/users/alice",
            "object": { "type": "Like" },
        });
        assert!(
            ApActivities::deserialize(&idless).is_err(),
            "no id anywhere => parse failure => 400 by design"
        );
    }
}
