//! Phase 2 integration tests against a real Postgres (migration + model layer).
//!
//! The test database is dropped and recreated on every run. Serialized via a
//! static mutex so each `#[tokio::test]` gets a pristine schema.
//!
//! Connection parameters come only from `TOOTTOK_TEST_DB`. A unique database
//! name is derived from the process id, created through the maintenance
//! `postgres` database, migrated, and torn down on the next run. No psql
//! binary is involved. When Postgres is unreachable the suite panics loudly
//! unless `TOOTTOK_TEST_SKIP=1`, in which case tests are skipped explicitly.

use std::collections::HashSet;
use std::sync::OnceLock;

use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use toottok_db::activity::Activity;
use toottok_db::actor::Actor;
use toottok_db::announce::Announce;
use toottok_db::clip::Clip;
use toottok_db::comment::Comment;
use toottok_db::follow::Follow;
use toottok_db::feed;
use toottok_db::job::Job;
use toottok_db::like::Like;
use toottok_db::settings::Setting;
use toottok_db::user::User;

const DEFAULT_TEST_URL: &str = "postgres://toottok:toottok@127.0.0.1:5433/toottok_test";

/// Serializes every test; each one drops/recreates the schema.
fn test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Drop + recreate a per-process test database, then run migrations. Returns
/// `None` (after printing a hint) only when `TOOTTOK_TEST_SKIP=1`; otherwise
/// any setup failure panics so a missing database is never silently ignored.
async fn setup() -> Option<sqlx::PgPool> {
    match setup_inner().await {
        Ok(pool) => Some(pool),
        Err(e) => {
            if std::env::var("TOOTTOK_TEST_SKIP").as_deref() == Ok("1") {
                eprintln!("toottok-db test setup failed ({e}); TOOTTOK_TEST_SKIP=1 set, skipping");
                None
            } else {
                panic!("toottok-db test setup failed: {e}");
            }
        }
    }
}

async fn setup_inner() -> Result<sqlx::PgPool, Box<dyn std::error::Error>> {
    let url = std::env::var("TOOTTOK_TEST_DB").unwrap_or_else(|_| DEFAULT_TEST_URL.to_string());
    let options: PgConnectOptions = url.parse()?;
    let db_name = format!("toottok_test_{}", std::process::id());

    let maintenance = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone().database("postgres"))
        .await?;

    for sql in [
        format!("DROP DATABASE IF EXISTS {db_name} WITH (FORCE);"),
        format!("CREATE DATABASE {db_name};"),
    ] {
        sqlx::query(&sql).execute(&maintenance).await?;
    }
    maintenance.close().await;

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(options.clone().database(&db_name))
        .await?;
    toottok_db::migrate(&pool).await?;
    Ok(pool)
}

async fn make_actor(pool: &sqlx::PgPool, username: &str) -> Actor {
    let ap_id = format!("https://{username}.example/actor/{username}");
    make_actor_full(pool, username, None, &ap_id).await
}

async fn make_actor_full(
    pool: &sqlx::PgPool,
    username: &str,
    domain: Option<&str>,
    ap_id: &str,
) -> Actor {
    Actor::create(
        pool,
        username,
        domain,
        "person",
        "PUBKEY",
        None,
        &format!("{ap_id}/inbox"),
        None,
        &format!("{ap_id}/outbox"),
        &format!("{ap_id}/followers"),
        ap_id,
    )
    .await
    .expect("actor insert should succeed")
}

#[tokio::test]
async fn actor_and_user_create() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let actor = make_actor(&pool, "alice").await;
    assert!(actor.id > 0);
    assert_eq!(actor.username, "alice");
    assert_eq!(actor.actor_type, "person");
    assert!(actor.domain.is_none());

    let user = User::create(&pool, actor.id, Some("alice@example.com"), "argon2-hash")
        .await
        .unwrap();
    assert_eq!(user.actor_id, actor.id);
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
    assert!(!user.is_admin);

    let fetched_actor = Actor::fetch_by_id(&pool, actor.id)
        .await
        .unwrap()
        .expect("actor should be fetched");
    assert_eq!(fetched_actor.ap_id, actor.ap_id);

    let fetched_user = User::fetch_by_id(&pool, user.id)
        .await
        .unwrap()
        .expect("user should be fetched");
    assert_eq!(fetched_user.password_hash.as_deref(), Some("argon2-hash"));
    assert_eq!(fetched_user.status, "active");
}

#[tokio::test]
async fn local_clip_create() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let actor = make_actor(&pool, "bob").await;
    let clip = Clip::create_local(
        &pool,
        actor.id,
        "https://toot.local/clips/1",
        Some("<p>hello</p>"),
        "public",
        "ready",
        None,
    )
    .await
    .unwrap();

    assert_eq!(clip.origin, "local");
    assert_eq!(clip.visibility, "public");
    assert_eq!(clip.status, "ready");
    assert_eq!(clip.remote_media_url, None);

    let fetched = Clip::fetch_by_id(&pool, clip.id)
        .await
        .unwrap()
        .expect("clip should be fetched");
    assert_eq!(fetched.ap_id, "https://toot.local/clips/1");
}

#[tokio::test]
async fn duplicate_clip_ap_id_rejected() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let actor = make_actor(&pool, "charlie").await;
    let ap_id = "https://toot.local/clips/dup";
    Clip::create_local(&pool, actor.id, ap_id, None, "public", "ready", None)
        .await
        .unwrap();

    let dup = Clip::create_local(&pool, actor.id, ap_id, None, "public", "ready", None).await;
    assert!(dup.is_err(), "duplicate clips.ap_id must be rejected");
}

#[tokio::test]
async fn actor_username_partial_unique() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    make_actor_full(&pool, "dana", None, "https://dana.example/actor/dana").await;

    let dup = Actor::create(
        &pool,
        "dana",
        None,
        "person",
        "PUBKEY",
        None,
        "https://dana.example/actor/dana-2/inbox",
        None,
        "https://dana.example/actor/dana-2/outbox",
        "https://dana.example/actor/dana-2/followers",
        "https://dana.example/actor/dana-2",
    )
    .await;
    assert!(
        dup.is_err(),
        "second local actor with the same username must be rejected by the partial unique index"
    );

    let remote = make_actor_full(
        &pool,
        "dana",
        Some("remote.example"),
        "https://remote.example/actor/dana",
    )
    .await;
    assert_eq!(remote.username, "dana");
    assert_eq!(remote.domain.as_deref(), Some("remote.example"));
}

#[tokio::test]
async fn follows_unique_pair_enforced() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let follower = make_actor(&pool, "erin").await;
    let target = make_actor(&pool, "frank").await;

    Follow::create(&pool, follower.id, target.id, None, "accepted")
        .await
        .unwrap();

    let dup = Follow::create(&pool, follower.id, target.id, None, "accepted").await;
    assert!(dup.is_err(), "duplicate follow pair must be rejected");

    let fetched = Follow::fetch_by_pair(&pool, follower.id, target.id)
        .await
        .unwrap()
        .expect("follow should be fetched");
    assert_eq!(fetched.state, "accepted");
}

#[tokio::test]
async fn jobs_queue_skip_locked_works() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let job = Job::create(
        &pool,
        "deliver",
        &serde_json::json!({"activity": "https://x/a/1"}),
        None,
    )
    .await
    .unwrap();

    let mut tx = pool.begin().await.unwrap();
    let claimed = Job::claim_next_due_tx(&mut tx, "worker-1")
        .await
        .unwrap()
        .expect("one queued job is due");
    assert_eq!(claimed.id, job.id);
    assert_eq!(claimed.state, "running");
    assert_eq!(claimed.locked_by.as_deref(), Some("worker-1"));

    let second = Job::claim_next_due(&pool, "worker-2").await.unwrap();
    assert!(
        second.is_none(),
        "row locked by the uncommitted worker-1 tx must be skipped"
    );

    tx.commit().await.unwrap();

    let locked = Job::fetch_by_id(&pool, job.id)
        .await
        .unwrap()
        .expect("job should be fetched");
    assert_eq!(locked.state, "running");
    assert_eq!(locked.locked_by.as_deref(), Some("worker-1"));
}

#[tokio::test]
async fn jobs_queue_concurrent_claims() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let mut seeded = HashSet::new();
    for i in 0..3 {
        let job = Job::create(&pool, "deliver", &serde_json::json!({"n": i}), None)
            .await
            .unwrap();
        seeded.insert(job.id);
    }

    let mut handles = Vec::new();
    for w in 0..8 {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            Job::claim_next_due(&pool, &format!("worker-{w}")).await
        }));
    }

    let mut winners = HashSet::new();
    for handle in handles {
        let claimed = handle.await.expect("claim task panicked");
        if let Some(job) = claimed.expect("claim should succeed") {
            winners.insert(job.id);
        }
    }

    assert_eq!(winners.len(), 3, "exactly the three seeded jobs win");
    assert!(
        winners.iter().all(|id| seeded.contains(id)),
        "every winner must be one of the seeded jobs"
    );
}

#[tokio::test]
async fn activities_duplicate_activity_id_rejected() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let raw = serde_json::json!({"type": "Create", "object": "https://x/o/1"});
    let activity = Activity::create_inbound(
        &pool,
        "https://x/activities/1",
        "https://x/actor/1",
        Some("https://x/o/1"),
        &raw,
    )
    .await
    .unwrap();
    assert_eq!(activity.direction, "inbound");

    let dup = Activity::create_inbound(
        &pool,
        "https://x/activities/1",
        "https://x/actor/1",
        Some("https://x/o/1"),
        &raw,
    )
    .await;
    assert!(
        dup.is_err(),
        "second insert with the same activity_id must be rejected"
    );

    let fetched = Activity::fetch_by_id(&pool, activity.id)
        .await
        .unwrap()
        .expect("activity should be fetched");
    assert_eq!(fetched.activity_id, "https://x/activities/1");
}

#[tokio::test]
async fn actor_fetch_by_ap_id() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let actor = make_actor(&pool, "grace").await;
    let fetched = Actor::fetch_by_ap_id(&pool, &actor.ap_id)
        .await
        .unwrap()
        .expect("actor should be fetched by ap_id");
    assert_eq!(fetched.id, actor.id);
    assert_eq!(fetched.ap_id, actor.ap_id);

    let missing = Actor::fetch_by_ap_id(&pool, "https://nope.example/actor/x")
        .await
        .unwrap();
    assert!(missing.is_none());
}

#[tokio::test]
async fn setting_upsert_round_trip() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let first = Setting::set(&pool, "site_name", &serde_json::json!("TootTok"))
        .await
        .unwrap();
    assert_eq!(first.key, "site_name");
    assert_eq!(first.value, serde_json::json!("TootTok"));

    let second = Setting::set(&pool, "site_name", &serde_json::json!("New Name"))
        .await
        .unwrap();
    assert_eq!(second.value, serde_json::json!("New Name"));

    let fetched = Setting::fetch_by_key(&pool, "site_name")
        .await
        .unwrap()
        .expect("setting should be fetched");
    assert_eq!(fetched.value, serde_json::json!("New Name"));
}

#[tokio::test]
async fn comment_like_announce_happy_paths() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    let actor = make_actor(&pool, "hank").await;
    let clip = Clip::create_local(
        &pool,
        actor.id,
        "https://toot.local/clips/hank",
        None,
        "public",
        "ready",
        None,
    )
    .await
    .unwrap();

    let comment = Comment::create(
        &pool,
        clip.id,
        actor.id,
        None,
        "https://toot.local/comments/1",
        "<p>nice clip</p>",
    )
    .await
    .unwrap();
    assert_eq!(comment.clip_id, clip.id);
    assert_eq!(comment.body_html, "<p>nice clip</p>");

    let fetched_comment = Comment::fetch_by_id(&pool, comment.id)
        .await
        .unwrap()
        .expect("comment should be fetched");
    assert_eq!(fetched_comment.ap_id, "https://toot.local/comments/1");

    let like = Like::create(&pool, clip.id, actor.id, Some("https://toot.local/likes/1"))
        .await
        .unwrap();
    assert_eq!(like.clip_id, clip.id);
    assert_eq!(like.actor_id, actor.id);

    let fetched_like = Like::fetch_by_pair(&pool, clip.id, actor.id)
        .await
        .unwrap()
        .expect("like should be fetched");
    assert_eq!(fetched_like.actor_id, actor.id);

    let announce = Announce::create(
        &pool,
        clip.id,
        actor.id,
        Some("https://toot.local/announces/1"),
    )
    .await
    .unwrap();
    assert_eq!(announce.clip_id, clip.id);

    let fetched_announce = Announce::fetch_by_pair(&pool, clip.id, actor.id)
        .await
        .unwrap()
        .expect("announce should be fetched");
    assert_eq!(fetched_announce.actor_id, actor.id);
}

#[tokio::test]
async fn local_feed_excludes_remote_actors() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    // Local author with a ready clip.
    let local_actor = make_actor(&pool, "gina").await;
    Clip::create_local(
        &pool,
        local_actor.id,
        "https://toot.local/clips/gina",
        None,
        "public",
        "ready",
        None,
    )
    .await
    .unwrap();

    // Remote actor (domain set) with a cached remote clip.
    let remote_actor = make_actor_full(
        &pool,
        "faraway",
        Some("remote.example"),
        "https://remote.example/users/faraway",
    )
    .await;
    Clip::create_remote(
        &pool,
        remote_actor.id,
        "https://remote.example/videos/9",
        None,
        Some(1.0),
        Some(720),
        Some(1280),
        "https://remote.example/videos/9/720.mp4",
        false,
        None,
        None,
    )
    .await
    .unwrap();

    // Local timeline: only the local author's clip.
    let local_rows = feed::local_feed(&pool, None, None, 20).await.unwrap();
    assert_eq!(local_rows.len(), 1, "local timeline must exclude remote actors");
    assert_eq!(local_rows[0].username, "gina");
    assert!(local_rows[0].domain.is_none());

    // Federated timeline: both.
    let all_rows = feed::discover_feed(&pool, None, None, 20).await.unwrap();
    assert_eq!(all_rows.len(), 2, "discover includes local + remote");

    // Keyset cursor still works on the local feed.
    let first = &local_rows[0];
    let paged = feed::local_feed(
        &pool,
        Some(first.clip_created_at),
        Some(first.id),
        20,
    )
    .await
    .unwrap();
    assert!(paged.is_empty(), "cursor past the single clip yields nothing");
}
