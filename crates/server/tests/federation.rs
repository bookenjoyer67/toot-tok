//! Phase 5 wave A cross-instance federation test.
//!
//! Spawns TWO real toottok servers on ephemeral loopback ports (the crate
//! example's `localhost:port` trick: each instance's federation domain is
//! `localhost:<port>`, so `is_local_url` keeps the two instances distinct while
//! DNS trivially resolves `localhost` for both the crate's client and our
//! egress guard). The egress guard is switched to
//! `allow_insecure_loopback_peers` so it permits loopback + http.
//!
//! Scripted round-trip: A registers alice, B registers bob, alice follows bob
//! by URI → Follow delivered signed → B auto-accepts → Accept delivered signed
//! → both sides record `follows.state = accepted`. Then unfollow propagates
//! (Undo) and the follow rows disappear on both sides. Idempotency: replaying
//! the same Follow activity (new signature) is skipped, not duplicated.
//!
//! Wave B adds `clip_create_delete_round_trip`: bob uploads a real 720p clip
//! on B (after alice follows him) → finalize builds Create(Note) → signed
//! fan-out to alice's shared inbox → A caches a remote-origin clip row
//! (no media assets, no transcode jobs). Then the clip is deleted on B and
//! the tombstone flips A's row; replaying the Create stays idempotent.
//!
//! Skips (with a hint) when `TOOTTOK_TEST_SKIP=1`; otherwise a missing database
//! is a hard panic.

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::PgPool;
use toottok::AppState;
use toottok_media::store::{LocalStore, Store};

const DEFAULT_TEST_URL: &str = "postgres://toottok:toottok@127.0.0.1:5433/toottok_test";

/// Serializes the two-instance tests (they share the DB server).
fn test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// A running instance under test.
#[allow(dead_code)]
struct Instance {
    pool: PgPool,
    /// Public base URL: `http://127.0.0.1:{port}`.
    addr: String,
    /// Federation domain: `localhost:{port}`.
    domain: String,
    port: u16,
    /// The instance's egress guard (used for direct delivery in assertions).
    egress: toottok_federation::EgressGuard,
}

async fn setup() -> Option<(Instance, Instance)> {
    match setup_inner().await {
        Ok(v) => Some(v),
        Err(e) => {
            if std::env::var("TOOTTOK_TEST_SKIP").as_deref() == Ok("1") {
                eprintln!("federation test setup failed ({e}); TOOTTOK_TEST_SKIP=1 set, skipping");
                None
            } else {
                panic!("federation test setup failed: {e}");
            }
        }
    }
}

async fn create_db(url: &str, db_name: &str) -> Result<PgPool, Box<dyn std::error::Error>> {
    let options: PgConnectOptions = url.parse()?;
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
        .connect_with(options.clone().database(db_name))
        .await?;
    toottok_db::migrate(&pool).await?;
    Ok(pool)
}

async fn setup_inner() -> Result<(Instance, Instance), Box<dyn std::error::Error>> {
    let url = std::env::var("TOOTTOK_TEST_DB").unwrap_or_else(|_| DEFAULT_TEST_URL.to_string());
    let pid = std::process::id();
    let pool_a = create_db(&url, &format!("toottok_fed_a_{pid}")).await?;
    let pool_b = create_db(&url, &format!("toottok_fed_b_{pid}")).await?;
    let (a, b) = tokio::try_join!(spawn_instance(pool_a, "a"), spawn_instance(pool_b, "b"),)?;
    Ok((a, b))
}

/// Bind an ephemeral loopback listener, build a full toottok app + workers for
/// that pool, and serve it on the chosen port.
async fn spawn_instance(pool: PgPool, _name: &str) -> Result<Instance, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let cfg = toottok::config::Config {
        domain: "localhost".to_string(),
        public_port: Some(port),
        allow_insecure_loopback_peers: true, // dev/test escape hatch
        worker_concurrency: 2,
        ..toottok::config::Config::default()
    };

    let tmp = tempfile::tempdir()?;
    let store: Arc<dyn Store> = Arc::new(LocalStore::new(tmp.path().join("media")));
    let mut state = AppState::new(Some(pool.clone()), store, cfg);
    state.init_federation().await?;

    let app = toottok::app(state.clone());
    let egress = state.egress.clone();
    toottok::worker::spawn_worker_pool(
        pool.clone(),
        state.store.clone(),
        2,
        Duration::from_secs(900),
        egress.clone(),
        format!("http://localhost:{port}"),
    )
    .await;
    toottok::worker::spawn_delivery_worker(pool.clone(), egress).await;

    tokio::spawn(async move {
        let _tmp = tmp;
        if let Err(e) = axum::serve(listener, app.into_make_service()).await {
            eprintln!("instance server exited: {e}");
        }
    });

    Ok(Instance {
        pool,
        addr: format!("http://127.0.0.1:{port}"),
        domain: format!("localhost:{port}"),
        port,
        egress: state.egress,
    })
}

/// ── HTTP helpers (against the real servers) ─────────────────────────────────
async fn http_post_json(
    addr: &str,
    path: &str,
    body: Value,
    cookie: Option<&str>,
    csrf: Option<&str>,
) -> (reqwest::StatusCode, Value) {
    let client = reqwest::Client::new();
    let mut req = client.post(format!("{addr}{path}")).json(&body);
    if let Some(c) = cookie {
        req = req.header("cookie", c);
    }
    if let Some(t) = csrf {
        req = req.header("x-toottok-csrf", t);
    }
    let resp = req.send().await.expect("http post");
    let status = resp.status();
    let body = resp.json::<Value>().await.unwrap_or_else(|_| json!({}));
    (status, body)
}

async fn http_get(
    addr: &str,
    path: &str,
) -> (reqwest::StatusCode, Value, reqwest::header::HeaderMap) {
    http_get_accept(addr, path, Some("application/activity+json")).await
}

/// GET with an explicit `Accept` header (pass `None` to send none at all).
async fn http_get_accept(
    addr: &str,
    path: &str,
    accept: Option<&str>,
) -> (reqwest::StatusCode, Value, reqwest::header::HeaderMap) {
    let client = reqwest::Client::new();
    let mut req = client.get(format!("{addr}{path}"));
    if let Some(a) = accept {
        req = req.header("accept", a);
    }
    let resp = req.send().await.expect("http get");
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = resp.json::<Value>().await.unwrap_or_else(|_| json!({}));
    (status, body, headers)
}

/// Authenticated multipart upload of raw clip bytes with an optional caption.
async fn upload_clip(
    addr: &str,
    cookie: &str,
    csrf: &str,
    filename: &str,
    bytes: &[u8],
    caption: Option<&str>,
) -> (reqwest::StatusCode, Value) {
    let boundary = "toottok-fed-test-boundary";
    let mut body: Vec<u8> = Vec::new();
    if let Some(c) = caption {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"caption_html\"\r\n\r\n");
        body.extend_from_slice(c.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: video/mp4\r\n\r\n");
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{addr}/api/v1/clips/upload"))
        .header("cookie", cookie)
        .header("x-toottok-csrf", csrf)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .expect("upload post");
    let status = resp.status();
    let body = resp.json::<Value>().await.unwrap_or_else(|_| json!({}));
    (status, body)
}

/// A 1s 720x1280 mp4 (video + AAC audio) built with ffmpeg — tall enough that
/// the transcode ladder produces the `720` rung the Create attachment points
/// at. `None` when ffmpeg is unavailable.
fn fixture_clip_720(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("ffmpeg not found; skipping fixture-dependent clip federation test");
        return None;
    }
    let path = dir.join("fixture-720x1280.mp4");
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("testsrc=duration=1:size=720x1280:rate=10")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("anullsrc=r=44100:cl=stereo")
        .arg("-c:v")
        .arg("libx264")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg("-c:a")
        .arg("aac")
        .arg("-shortest")
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    status.success().then_some(path)
}

/// Register + login `username`; returns `(cookie, csrf_token)`.
async fn register_and_login(addr: &str, username: &str) -> (String, String) {
    let (status, body) = http_post_json(
        addr,
        "/api/v1/auth/register",
        json!({ "username": username, "password": "password123" }),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::CREATED,
        "register {username}: {body:?}"
    );

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{addr}/api/v1/auth/login"))
        .json(&json!({ "username_or_email": username, "password": "password123" }))
        .send()
        .await
        .expect("login");
    assert_eq!(resp.status(), reqwest::StatusCode::OK, "login {username}");
    let cookie = resp
        .headers()
        .get("set-cookie")
        .expect("set-cookie")
        .to_str()
        .expect("cookie")
        .split(';')
        .next()
        .expect("segment")
        .to_string();
    let body: Value = resp.json().await.expect("login body");
    let csrf = body["csrf_token"].as_str().expect("csrf_token").to_string();
    (cookie, csrf)
}

async fn follow_state(pool: &PgPool, follower_ap: &str, target_ap: &str) -> Option<String> {
    sqlx::query_scalar(
        r#"
        SELECT f.state FROM follows f
        JOIN actors fa ON fa.id = f.follower_actor_id
        JOIN actors ta ON ta.id = f.target_actor_id
        WHERE fa.ap_id = $1 AND ta.ap_id = $2
        "#,
    )
    .bind(follower_ap)
    .bind(target_ap)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

async fn wait_until<F, Fut>(timeout: Duration, what: &str, f: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if f().await {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for: {what}");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn bob_uri(inst: &Instance) -> String {
    format!("http://localhost:{}/users/bob", inst.port)
}

fn alice_uri(inst: &Instance) -> String {
    format!("http://localhost:{}/users/alice", inst.port)
}

/// The full scripted round-trip in one test: register both sides, follow,
/// accept, unfollow, plus idempotency and activity-log assertions.
#[tokio::test]
async fn follow_accept_unfollow_round_trip() {
    let _guard = test_lock().lock().await;
    let Some((a, b)) = setup().await else {
        return;
    };

    // Endpoints are live.
    let (status, body, _) = http_get(&a.addr, "/ap/actor").await;
    assert_eq!(status, reqwest::StatusCode::OK, "instance actor served");
    assert_eq!(body["type"], "Application");

    let (status, body, _) = http_get(&a.addr, "/.well-known/nodeinfo").await;
    assert_eq!(status, reqwest::StatusCode::OK, "nodeinfo jrd served");
    assert_eq!(
        body["links"].as_array().map(Vec::len),
        Some(2),
        "both nodeinfo versions advertised"
    );
    for link in body["links"].as_array().unwrap() {
        let href = link["href"].as_str().unwrap();
        let (status, doc, _) = http_get(href, "").await;
        assert_eq!(status, reqwest::StatusCode::OK, "nodeinfo doc {href}");
        assert_eq!(doc["software"]["name"], "toottok");
    }

    // Register + login both sides.
    let (alice_cookie, alice_csrf) = register_and_login(&a.addr, "alice").await;
    let (bob_cookie, _bob_csrf) = register_and_login(&b.addr, "bob").await;
    let _ = bob_cookie;

    // alice's actor is served with a key + inbox.
    let (status, alice_actor, _) = http_get(&a.addr, "/users/alice").await;
    assert_eq!(status, reqwest::StatusCode::OK, "alice person served");
    assert_eq!(alice_actor["id"], alice_uri(&a));
    assert!(alice_actor["publicKey"]["publicKeyPem"].as_str().is_some());

    // A follows B's user by URI (authed + CSRF).
    let (status, body) = http_post_json(
        &a.addr,
        "/api/v1/follows",
        json!({ "actor_uri": bob_uri(&b) }),
        Some(&alice_cookie),
        Some(&alice_csrf),
    )
    .await;
    assert_eq!(
        status,
        reqwest::StatusCode::ACCEPTED,
        "follow accepted: {body:?}"
    );

    // Wait for the round trip: accepted on BOTH sides.
    wait_until(Duration::from_secs(20), "accept on both sides", || async {
        let a_side = follow_state(&a.pool, &alice_uri(&a), &bob_uri(&b)).await;
        let b_side = follow_state(&b.pool, &alice_uri(&a), &bob_uri(&b)).await;
        a_side.as_deref() == Some("accepted") && b_side.as_deref() == Some("accepted")
    })
    .await;

    // Activities logged on both sides.
    let a_activities: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM activities")
        .fetch_one(&a.pool)
        .await
        .expect("a activities");
    let b_activities: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM activities")
        .fetch_one(&b.pool)
        .await
        .expect("b activities");
    assert!(
        a_activities >= 2,
        "A logged outbound Follow + inbound Accept (got {a_activities})"
    );
    assert!(
        b_activities >= 2,
        "B logged inbound Follow + outbound Accept (got {b_activities})"
    );

    // B cached alice's actor row (fetched during signature verification).
    let cached: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM actors WHERE ap_id = $1)")
        .bind(alice_uri(&a))
        .fetch_one(&b.pool)
        .await
        .expect("actor lookup");
    assert!(cached, "B has alice cached");

    // Instances bookkeeping: both sides recorded the other.
    let instances_a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instances")
        .fetch_one(&a.pool)
        .await
        .expect("a instances");
    let instances_b: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM instances")
        .fetch_one(&b.pool)
        .await
        .expect("b instances");
    assert!(instances_a >= 1, "A recorded an instance");
    assert!(instances_b >= 1, "B recorded an instance");

    // ── Idempotency: replay the SAME Follow activity (fresh signature) ────────
    // Read the raw Follow B stored for the inbound delivery and the follow id.
    let stored: Value = sqlx::query_scalar(
        "SELECT raw FROM activities WHERE direction = 'inbound' AND raw->>'type' = 'Follow' LIMIT 1",
    )
    .fetch_one(&b.pool)
    .await
    .expect("stored follow raw");
    let follow_id = stored["id"].as_str().expect("follow id").to_string();

    let alice = toottok_db::actor::Actor::fetch_by_ap_id(&a.pool, &alice_uri(&a))
        .await
        .expect("alice row")
        .expect("alice exists");
    let inbox = format!("{}/ap/inbox", b.addr);
    for _ in 0..2 {
        let outcome = toottok_federation::deliver::deliver_activity(
            &a.pool,
            &a.egress,
            &alice,
            &reqwest::Url::parse(&inbox).expect("inbox url"),
            &stored,
        )
        .await
        .expect("replay delivery");
        match outcome {
            toottok_federation::deliver::DeliverOutcome::Delivered
            | toottok_federation::deliver::DeliverOutcome::Rejected(_) => {}
            other => panic!("replay should not retry: {other:?}"),
        }
    }

    // Not duplicated: one stored row, one follows row.
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM activities WHERE activity_id = $1")
        .bind(&follow_id)
        .fetch_one(&b.pool)
        .await
        .expect("activity count");
    assert_eq!(count, 1, "replayed follow was idempotently skipped");
    let follows: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*) FROM follows f
        JOIN actors fa ON fa.id = f.follower_actor_id
        JOIN actors ta ON ta.id = f.target_actor_id
        WHERE fa.ap_id = $1 AND ta.ap_id = $2
        "#,
    )
    .bind(alice_uri(&a))
    .bind(bob_uri(&b))
    .fetch_one(&b.pool)
    .await
    .expect("follow count");
    assert_eq!(follows, 1, "follow row not duplicated by replay");

    // ── Unfollow propagates (Undo) ────────────────────────────────────────────
    let bob_id: i64 = sqlx::query_scalar("SELECT id FROM actors WHERE ap_id = $1")
        .bind(bob_uri(&b))
        .fetch_one(&a.pool)
        .await
        .expect("bob actor id on A");

    let (status, _) = http_post_json(
        &a.addr,
        &format!("/api/v1/follows/{bob_id}/unfollow"),
        json!({}),
        Some(&alice_cookie),
        Some(&alice_csrf),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::NO_CONTENT, "unfollow");

    wait_until(
        Duration::from_secs(20),
        "follow rows gone on both sides",
        || async {
            let a_side = follow_state(&a.pool, &alice_uri(&a), &bob_uri(&b)).await;
            let b_side = follow_state(&b.pool, &alice_uri(&a), &bob_uri(&b)).await;
            a_side.is_none() && b_side.is_none()
        },
    )
    .await;

    // B logged the inbound Undo.
    let undo_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM activities WHERE direction = 'inbound' AND raw->>'type' = 'Undo'",
    )
    .fetch_one(&b.pool)
    .await
    .expect("undo count");
    assert_eq!(undo_count, 1, "B received and stored one Undo");

    // webfinger resolves the local user.
    let wf_resource = format!("acct:alice@localhost:{}", a.port);
    let (status, wf, _) = http_get(
        &a.addr,
        &format!("/.well-known/webfinger?resource={wf_resource}"),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK, "webfinger for alice");
    assert_eq!(wf["subject"], wf_resource);
}

/// ── Phase 5 wave B: clip federation ──────────────────────────────────────────
///
/// Full cross-instance clip lifecycle: follow BEFORE upload → B finalizes a
/// real 720p upload → Create(Note) fans out signed as the AUTHOR to alice's
/// shared inbox → A caches a remote-origin row (hot-linked media, no local
/// assets, no transcode jobs) → delete propagates → replays stay idempotent.
#[tokio::test]
async fn clip_create_delete_round_trip() {
    let _guard = test_lock().lock().await;
    let Some((a, b)) = setup().await else {
        return;
    };

    // Register + login both sides.
    let (alice_cookie, alice_csrf) = register_and_login(&a.addr, "alice").await;
    let (bob_cookie, bob_csrf) = register_and_login(&b.addr, "bob").await;

    // Alice follows Bob BEFORE the upload, so B's follower fan-out finds her.
    let (status, body) = http_post_json(
        &a.addr,
        "/api/v1/follows",
        json!({ "actor_uri": bob_uri(&b) }),
        Some(&alice_cookie),
        Some(&alice_csrf),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED, "follow: {body:?}");
    wait_until(
        Duration::from_secs(20),
        "follow accepted on both sides",
        || async {
            let a_side = follow_state(&a.pool, &alice_uri(&a), &bob_uri(&b)).await;
            let b_side = follow_state(&b.pool, &alice_uri(&a), &bob_uri(&b)).await;
            a_side.as_deref() == Some("accepted") && b_side.as_deref() == Some("accepted")
        },
    )
    .await;

    // Bob uploads a REAL 720p clip (with caption) on B.
    let tmp = tempfile::tempdir().expect("tempdir");
    let Some(fixture) = fixture_clip_720(tmp.path()) else {
        return;
    };
    let bytes = std::fs::read(&fixture).expect("fixture bytes");
    let caption = "bob's first federated clip";
    let (status, body) = upload_clip(
        &b.addr,
        &bob_cookie,
        &bob_csrf,
        "clip.mp4",
        &bytes,
        Some(caption),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::CREATED, "upload: {body:?}");
    let b_clip_id = body["clip_id"].as_i64().expect("clip id in response");

    let b_clip_ap = format!("http://localhost:{}/clips/{b_clip_id}", b.port);
    let b_media_url = format!("http://localhost:{}/assets/{b_clip_id}/720.mp4", b.port);

    // Wait for B's probe→transcode→finalize→deliver_create→deliver chain to
    // land the Create on A.
    wait_until(
        Duration::from_secs(120),
        "remote clip cached on A",
        || async {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM clips WHERE origin = 'remote' AND ap_id = $1)",
            )
            .bind(&b_clip_ap)
            .fetch_one(&a.pool)
            .await
            .unwrap_or(false)
        },
    )
    .await;

    // A-side row: remote origin, born ready, hot-linked media, no sha256.
    let (row_status, deleted): (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, deleted_at FROM clips WHERE ap_id = $1")
            .bind(&b_clip_ap)
            .fetch_one(&a.pool)
            .await
            .expect("remote clip row");
    assert_eq!(row_status, "ready");
    assert!(deleted.is_none(), "fresh remote clip is not deleted");

    let remote_media: Option<String> =
        sqlx::query_scalar("SELECT remote_media_url FROM clips WHERE ap_id = $1")
            .bind(&b_clip_ap)
            .fetch_one(&a.pool)
            .await
            .expect("remote_media_url");
    assert_eq!(
        remote_media.as_deref(),
        Some(b_media_url.as_str()),
        "media points at B's public 720p asset URL"
    );

    let sha: Option<String> = sqlx::query_scalar("SELECT sha256_hash FROM clips WHERE ap_id = $1")
        .bind(&b_clip_ap)
        .fetch_one(&a.pool)
        .await
        .expect("sha256");
    assert!(sha.is_none(), "remote rows never carry a dedup hash");

    let stored_caption: Option<String> =
        sqlx::query_scalar("SELECT caption_html FROM clips WHERE ap_id = $1")
            .bind(&b_clip_ap)
            .fetch_one(&a.pool)
            .await
            .expect("caption");
    assert_eq!(
        stored_caption.as_deref(),
        // F9 stance: captions store HTML-ESCAPED plain text (tags stripped,
        // remainder escaped) so no downstream renderer can revive markup.
        // Apostrophe -> &#x27; is the escaper working as designed.
        Some("bob&#x27;s first federated clip"),
        "caption preserved (escaped form)"
    );

    let dims: (Option<i32>, Option<i32>, Option<f64>) =
        sqlx::query_as("SELECT width, height, duration_s FROM clips WHERE ap_id = $1")
            .bind(&b_clip_ap)
            .fetch_one(&a.pool)
            .await
            .expect("dims");
    assert_eq!(dims.0, Some(720));
    assert_eq!(dims.1, Some(1280));
    assert!(
        dims.2.map(|d| (d - 1.0).abs() < 0.5).unwrap_or(false),
        "duration carried from attachment: {:?}",
        dims.2
    );

    let author_id: i64 = sqlx::query_scalar("SELECT actor_id FROM clips WHERE ap_id = $1")
        .bind(&b_clip_ap)
        .fetch_one(&a.pool)
        .await
        .expect("author");
    let cached_bob_id: i64 = sqlx::query_scalar("SELECT id FROM actors WHERE ap_id = $1")
        .bind(bob_uri(&b))
        .fetch_one(&a.pool)
        .await
        .expect("cached bob");
    assert_eq!(author_id, cached_bob_id, "attributed to the remote actor");

    // NO local media pipeline ran for the remote clip.
    let assets_a: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_assets")
        .fetch_one(&a.pool)
        .await
        .expect("asset count");
    assert_eq!(assets_a, 0, "no media_assets rows locally");
    let media_jobs_a: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE kind IN ('probe', 'transcode', 'finalize')",
    )
    .fetch_one(&a.pool)
    .await
    .expect("job count");
    assert_eq!(media_jobs_a, 0, "no probe/transcode jobs for remote clips");

    // Wire JSON of the outbound Create on B: Loops shape with followers `to`,
    // public `cc`, and an mp4 Document attachment.
    let create_raw: Value = sqlx::query_scalar(
        r#"
        SELECT raw FROM activities
        WHERE direction = 'outbound' AND raw->>'type' = 'Create'
          AND raw->'object'->>'id' = $1
        "#,
    )
    .bind(&b_clip_ap)
    .fetch_one(&b.pool)
    .await
    .expect("stored outbound Create");
    assert_eq!(create_raw["object"]["type"], "Note");
    assert_eq!(create_raw["object"]["id"], json!(b_clip_ap));
    assert_eq!(create_raw["object"]["url"], json!(b_clip_ap));
    assert_eq!(
        create_raw["object"]["attributedTo"],
        json!(bob_uri(&b)),
        "signed and attributed as the AUTHOR"
    );
    assert_eq!(
        create_raw["to"],
        json!([format!("http://localhost:{}/users/bob/followers", b.port)]),
        "addressed to the author's followers collection"
    );
    assert!(
        create_raw["cc"]
            .as_array()
            .map(|cc| cc
                .iter()
                .any(|v| v == "https://www.w3.org/ns/activitystreams#Public"))
            .unwrap_or(false),
        "public cc present: {:?}",
        create_raw["cc"]
    );
    assert_eq!(create_raw["object"]["attachment"][0]["type"], "Document");
    assert_eq!(
        create_raw["object"]["attachment"][0]["mediaType"],
        "video/mp4"
    );
    assert_eq!(
        create_raw["object"]["attachment"][0]["url"],
        json!(b_media_url)
    );
    let create_id = create_raw["id"].as_str().expect("create id").to_string();

    // The AP object endpoints serve the same documents on B.
    let (status, note, _) = http_get(&b.addr, &format!("/clips/{b_clip_id}")).await;
    assert_eq!(status, reqwest::StatusCode::OK, "GET /clips/{{id}} note");
    assert_eq!(note["type"], "Note");
    assert_eq!(note["id"], json!(b_clip_ap));
    // F9: stored caption is escaped plain text; the Note `content` carries
    // that same escaped form (AP consumers render content as HTML).
    assert_eq!(note["content"], json!("bob&#x27;s first federated clip"));
    assert_eq!(
        note["attachment"][0]["mediaType"], "video/mp4",
        "player-visible inline attachment"
    );
    let (status, activity, _) = http_get(&b.addr, &format!("/clips/{b_clip_id}/activity")).await;
    assert_eq!(status, reqwest::StatusCode::OK, "GET activity wrapper");
    assert_eq!(activity["type"], "Create");
    assert_eq!(activity["object"]["id"], json!(b_clip_ap));

    // Content negotiation: a plain HTML client does NOT get AP JSON here.
    let (status, _, _) =
        http_get_accept(&b.addr, &format!("/clips/{b_clip_id}"), Some("text/html")).await;
    assert_eq!(
        status,
        reqwest::StatusCode::NOT_ACCEPTABLE,
        "no activity+json Accept, no Note payload"
    );

    // Idempotency: replaying the SAME Create (new signature) is skipped by
    // the activities gate — still one activity log row, still one clip.
    let bob_on_b = toottok_db::actor::Actor::fetch_by_ap_id(&b.pool, &bob_uri(&b))
        .await
        .expect("bob row")
        .expect("bob exists");
    let inbox_a = format!("{}/ap/inbox", a.addr);
    for _ in 0..2 {
        let outcome = toottok_federation::deliver::deliver_activity(
            &b.pool,
            &b.egress,
            &bob_on_b,
            &reqwest::Url::parse(&inbox_a).expect("inbox url"),
            &create_raw,
        )
        .await
        .expect("replay delivery");
        assert!(
            matches!(
                outcome,
                toottok_federation::deliver::DeliverOutcome::Delivered
                    | toottok_federation::deliver::DeliverOutcome::Rejected(_)
            ),
            "replay must not be retried"
        );
    }
    let create_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM activities WHERE activity_id = $1")
            .bind(&create_id)
            .fetch_one(&a.pool)
            .await
            .expect("create activity count");
    assert_eq!(create_rows, 1, "replayed Create was idempotently skipped");
    let clip_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM clips WHERE ap_id = $1 AND deleted_at IS NULL")
            .bind(&b_clip_ap)
            .fetch_one(&a.pool)
            .await
            .expect("clip count after replay");
    assert_eq!(clip_rows, 1, "replay did not duplicate the clip");

    // ── Delete propagates: B tombstones the clip, A flips its row ────────────
    let delete_activity = json!({
        "@context": "https://www.w3.org/ns/activitystreams",
        "id": format!("http://localhost:{}/activities/{}", b.port, uuid::Uuid::new_v4()),
        "type": "Delete",
        "actor": bob_uri(&b),
        "object": { "id": b_clip_ap, "type": "Tombstone" },
        "to": [alice_uri(&a)],
    });
    let outcome = toottok_federation::deliver::deliver_activity(
        &b.pool,
        &b.egress,
        &bob_on_b,
        &reqwest::Url::parse(&inbox_a).expect("inbox url"),
        &delete_activity,
    )
    .await
    .expect("delete delivery");
    assert!(
        matches!(
            outcome,
            toottok_federation::deliver::DeliverOutcome::Delivered
        ),
        "delete should be accepted: {outcome:?}"
    );

    wait_until(
        Duration::from_secs(20),
        "A flipped the clip to deleted",
        || async {
            let deleted: Option<Option<chrono::DateTime<chrono::Utc>>> =
                sqlx::query_scalar("SELECT deleted_at FROM clips WHERE ap_id = $1")
                    .bind(&b_clip_ap)
                    .fetch_optional(&a.pool)
                    .await
                    .ok()
                    .flatten();
            matches!(deleted, Some(Some(_)))
        },
    )
    .await;
    let deleted_status: String = sqlx::query_scalar("SELECT status FROM clips WHERE ap_id = $1")
        .bind(&b_clip_ap)
        .fetch_one(&a.pool)
        .await
        .expect("deleted row");
    assert_eq!(deleted_status, "deleted", "status flipped with deleted_at");

    // And even a post-delete replay of the Create stays swallowed (tombstone).
    let _ = toottok_federation::deliver::deliver_activity(
        &b.pool,
        &b.egress,
        &bob_on_b,
        &reqwest::Url::parse(&inbox_a).expect("inbox url"),
        &create_raw,
    )
    .await;
    let clip_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM clips WHERE ap_id = $1 AND deleted_at IS NULL")
            .bind(&b_clip_ap)
            .fetch_one(&a.pool)
            .await
            .expect("live clip count");
    assert_eq!(clip_rows, 0, "delete wins over later Creates");
}
