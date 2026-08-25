//! Database-backed actor: implements the crate's `Object` + `Actor` traits over
//! the `actors` table (works for local persons, the instance Application actor,
//! and cached remote actors).

use std::sync::Arc;

use activitypub_federation::config::Data;
use activitypub_federation::fetch::object_id::ObjectId;
use activitypub_federation::protocol::context::WithContext;
use activitypub_federation::protocol::public_key::PublicKey;
use activitypub_federation::protocol::verification::verify_domains_match;
use activitypub_federation::traits::{Actor, Object};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use toottok_db::actor::Actor as DbActorRow;
use url::Url;

use crate::data::FederationData;
use crate::error::Error;

/// The `type` field of an ActivityPub actor (`Person` / `Application` /
/// `Service`), mapping 1:1 to `actors.actor_type`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum ApActorType {
    Person,
    Application,
    Service,
}

impl ApActorType {
    pub fn from_db(s: &str) -> Self {
        match s {
            "application" => ApActorType::Application,
            "service" => ApActorType::Service,
            _ => ApActorType::Person,
        }
    }

    pub fn as_db(&self) -> &'static str {
        match self {
            ApActorType::Person => "person",
            ApActorType::Application => "application",
            ApActorType::Service => "service",
        }
    }
}

/// Parse an ActivityPub actor document's `type` into our actor kind.
pub fn actor_type_from_json(value: &serde_json::Value) -> ApActorType {
    let kind = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Person");
    ApActorType::from_db(&match kind {
        "Application" => "application".to_string(),
        "Service" => "service".to_string(),
        _ => "person".to_string(),
    })
}

/// The parts of a remote actor document needed to build (or refresh) an
/// `actors` row.
pub struct RemoteActorParts {
    pub id: String,
    pub domain: String,
    pub username: String,
    pub actor_type: String,
    pub public_key_pem: String,
    pub inbox: String,
    pub shared_inbox: Option<String>,
    pub outbox: String,
    pub followers: String,
    /// Profile display name (`name` on the wire).
    pub display_name: Option<String>,
    /// Bio/summary (HTML; callers render as plain text).
    pub summary: Option<String>,
    /// Remote avatar URL from `icon.url` (hot-linked, not downloaded).
    pub avatar_url: Option<String>,
}

/// `endpoints` block of an ActivityPub actor (shared inbox).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorEndpoints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_inbox: Option<Url>,
}

/// Wire representation of an actor served at `/ap/actor` and `/users/{u}`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApActor {
    #[serde(rename = "type")]
    pub kind: ApActorType,
    pub id: ObjectId<DbActor>,
    pub preferred_username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub inbox: Url,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outbox: Option<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub followers: Option<Url>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<ActorEndpoints>,
    pub public_key: PublicKey,
}

/// Handle for one actor row, used by the crate as `ActorT` in inbox handlers
/// and as the `Kind` for `ObjectId<DbActor>` dereferences.
#[derive(Debug, Clone)]
pub struct DbActor {
    pub row: Arc<DbActorRow>,
    /// Parsed, immutable `ap_id` (the `Object::id` for this actor).
    pub ap_id: Url,
}

impl DbActor {
    pub fn from_row(row: DbActorRow) -> Self {
        let ap_id = Url::parse(&row.ap_id).expect("stored ap_id is a valid url");
        DbActor {
            row: Arc::new(row),
            ap_id,
        }
    }

    /// Serve an actor row as ActivityPub JSON, wrapped in `@context`.
    pub fn to_json(&self) -> WithContext<ApActor> {
        WithContext::new_default(ApActor {
            kind: ApActorType::from_db(&self.row.actor_type),
            id: ObjectId::parse(&self.row.ap_id).expect("stored ap_id is a valid url"),
            preferred_username: self.row.username.clone(),
            name: self.row.display_name.clone(),
            summary: self.row.summary.clone(),
            inbox: Url::parse(&self.row.inbox_url).expect("stored inbox url is valid"),
            outbox: Url::parse(&self.row.outbox_url).ok(),
            followers: Url::parse(&self.row.followers_url).ok(),
            endpoints: self.shared_inbox_url().map(|u| ActorEndpoints {
                shared_inbox: Some(u),
            }),
            public_key: Actor::public_key(self),
        })
    }

    /// The URL of this actor's shared inbox when the row carries one.
    pub fn shared_inbox_url(&self) -> Option<Url> {
        self.row
            .shared_inbox_url
            .as_deref()
            .and_then(|s| Url::parse(s).ok())
    }
}

#[async_trait]
impl Object for DbActor {
    type DataType = FederationData;
    type Kind = ApActor;
    type Error = Error;

    fn id(&self) -> &Url {
        &self.ap_id
    }

    fn last_refreshed_at(&self) -> Option<DateTime<Utc>> {
        Some(self.row.updated_at)
    }

    async fn read_from_id(
        object_id: Url,
        data: &Data<Self::DataType>,
    ) -> Result<Option<Self>, Self::Error> {
        let row = DbActorRow::fetch_by_ap_id(&data.pool, object_id.as_str()).await?;
        Ok(row.map(DbActor::from_row))
    }

    async fn into_json(self, _data: &Data<Self::DataType>) -> Result<Self::Kind, Self::Error> {
        Ok(self.to_json().inner().clone())
    }

    async fn verify(
        json: &Self::Kind,
        expected_domain: &Url,
        _data: &Data<Self::DataType>,
    ) -> Result<(), Self::Error> {
        verify_domains_match(json.id.inner(), expected_domain)?;
        if json.public_key.public_key_pem.is_empty() {
            return Err(Error::Other("actor has no public key".into()));
        }
        Ok(())
    }

    async fn from_json(json: Self::Kind, data: &Data<Self::DataType>) -> Result<Self, Self::Error> {
        let id = json.id.inner();
        let domain = id
            .host_str()
            .ok_or_else(|| Error::Other("actor id has no host".into()))?;
        let shared_inbox = json
            .endpoints
            .and_then(|e| e.shared_inbox)
            .map(|u| u.to_string());
        let row = DbActorRow::upsert_remote(
            &data.pool,
            &json.preferred_username,
            domain,
            json.kind.as_db(),
            &json.public_key.public_key_pem,
            json.inbox.as_str(),
            shared_inbox.as_deref(),
            json.outbox
                .as_ref()
                .map(Url::as_str)
                .unwrap_or_else(|| id.as_str()),
            json.followers
                .as_ref()
                .map(Url::as_str)
                .unwrap_or_else(|| id.as_str()),
            id.as_str(),
            json.name.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            json.summary
                .as_deref()
                .map(crate::note::strip_html_tags)
                .as_deref(),
            None,
        )
        .await?;
        Ok(DbActor::from_row(row))
    }
}

impl Actor for DbActor {
    fn public_key_pem(&self) -> &str {
        &self.row.public_key_pem
    }

    fn private_key_pem(&self) -> Option<String> {
        self.row.private_key_pem.clone()
    }

    fn inbox(&self) -> Url {
        Url::parse(&self.row.inbox_url).expect("stored inbox url is valid")
    }

    fn shared_inbox(&self) -> Option<Url> {
        self.shared_inbox_url()
    }
}
