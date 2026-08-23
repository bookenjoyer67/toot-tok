//! Phase 6 social API integration tests.
//!
//! Each test spins up its OWN fresh single-instance toottok server (per-pid,
//! per-test database names) against the real transcode workers, then exercises
//! the social surface over HTTP: follows → following feed → search → hashtag
//! rows, like/unlike idempotency, comments → notifications → mark-read →
//! soft-delete, reports → admin resolve, and the profile grid.
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

/// Serializes instance setup (they share the DB server).
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
    /// The instance's egress guard.
    egress: toottok_federation::EgressGuard,
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

async fn setup_inner(name: &str) -> Result<Instance, Box<dyn std::error::Error>> {
    let url = std::env::var("TOOTTOK_TEST_DB").unwrap_or_else(|_| DEFAULT_TEST_URL.to_string());
    let pid = std::process::id();
    let pool = create_db(&url, &format!("toottok_soc_{name}_{pid}")).await?;
    spawn_instance(pool, name).await
}

/// Fresh instance per test; panic-loud unless TOOTTOK_TEST_SKIP=1.
async fn setup(name: &str) -> Option<Instance> {
    let _guard = test_lock().lock().await;
    match setup_inner(name).await {
        Ok(v) => Some(v),
        Err(e) => {
            if std::env::var("TOOTTOK_TEST_SKIP").as_deref() == Ok("1") {
                eprintln!(
                    "social test `{name}` setup failed ({e}); TOOTTOK_TEST_SKIP=1 set, skipping"
                );
                None
            } else {
                panic!("social test `{name}` setup failed: {e}");
            }
        }
    }
}

/// ── HTTP helpers ────────────────────────────────────────────────────────────
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

async fn http_put_json(
    addr: &str,
    path: &str,
    body: Value,
    cookie: &str,
    csrf: &str,
) -> (reqwest::StatusCode, Value) {
    let client = reqwest::Client::new();
    let resp = client
        .put(format!("{addr}{path}"))
        .header("cookie", cookie)
        .header("x-toottok-csrf", csrf)
        .json(&body)
        .send()
        .await
        .expect("http put");
    let status = resp.status();
    let body = resp.json::<Value>().await.unwrap_or_else(|_| json!({}));
    (status, body)
}

async fn http_delete(
    addr: &str,
    path: &str,
    cookie: &str,
    csrf: &str,
) -> (reqwest::StatusCode, Value) {
    let client = reqwest::Client::new();
    let resp = client
        .delete(format!("{addr}{path}"))
        .header("cookie", cookie)
        .header("x-toottok-csrf", csrf)
        .send()
        .await
        .expect("http delete");
    let status = resp.status();
    let body = resp.json::<Value>().await.unwrap_or_else(|_| json!({}));
    (status, body)
}

async fn http_get_authed(
    addr: &str,
    path: &str,
    cookie: &str,
    csrf: &str,
) -> (reqwest::StatusCode, Value) {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{addr}{path}"))
        .header("cookie", cookie)
        .header("x-toottok-csrf", csrf)
        .send()
        .await
        .expect("http get");
    let status = resp.status();
    let body = resp.json::<Value>().await.unwrap_or_else(|_| json!({}));
    (status, body)
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
    let boundary = "toottok-social-test-boundary";
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

/// Create an `admin` superuser directly in the DB (real keypair, like the CLI).
async fn create_admin_directly(pool: &PgPool, port: u16) {
    let (pub_pem, priv_pem) = toottok::keys::generate_actor_keypair().expect("keypair");
    let admin_ap = format!("http://localhost:{port}/users/admin");
    let admin_actor = toottok_db::actor::Actor::create(
        pool,
        "admin",
        None,
        "person",
        &pub_pem,
        Some(&priv_pem),
        &format!("{admin_ap}/inbox"),
        Some(format!("{admin_ap}/inbox").as_str()),
        &format!("{admin_ap}/outbox"),
        &format!("{admin_ap}/followers"),
        &admin_ap,
    )
    .await
    .expect("admin actor");
    toottok_db::user::User::create_admin(
        pool,
        admin_actor.id,
        None,
        &toottok_db::password::hash_password("adminpassword123").expect("hash"),
    )
    .await
    .expect("admin user");
}

/// A 1s 720x1280 mp4 (video + AAC audio) built with ffmpeg lavfi. `hue`
/// shifts the pixels so two fixtures with different hues are never
/// content-identical (uploads are deduplicated server-side). `None` when
/// ffmpeg is unavailable.
fn fixture_clip_720(dir: &std::path::Path, name: &str, hue: i32) -> Option<std::path::PathBuf> {
    if Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("ffmpeg not found; skipping fixture-dependent social test");
        return None;
    }
    let path = dir.join(name);
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
        .arg("-vf")
        .arg(format!("hue=h={hue}"))
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

/// Poll until the closure returns `None` (done); otherwise records its
/// diagnostic string and retries, dumping the LAST diagnostic on timeout.
async fn wait_until<F, Fut>(timeout: Duration, every: Duration, what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let last = match f().await {
            None => return,
            Some(d) => d,
        };
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for: {what}; last poll: {last}");
        }
        tokio::time::sleep(every).await;
    }
}

/// Truncate a dumped payload for diagnostics.
fn snip(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 400 {
        format!("{}…", &s[..400])
    } else {
        s
    }
}

// ── response-shape helpers ──────────────────────────────────────────────────

/// Extract the list payload from a handler response regardless of whether it
/// is a bare JSON array or wrapped in a known envelope key.
fn as_items<'a>(body: &'a Value, keys: &[&str]) -> Vec<&'a Value> {
    if let Some(arr) = body.as_array() {
        return arr.iter().collect();
    }
    for k in keys {
        if let Some(arr) = body[*k].as_array() {
            return arr.iter().collect();
        }
    }
    Vec::new()
}

/// Pull an integer counter off a clip payload (top level or nested `clip`).
fn count_field(body: &Value, key: &str) -> Option<i64> {
    for v in [body, &body["clip"]] {
        if let Some(n) = v[key].as_i64() {
            return Some(n);
        }
    }
    None
}

/// Best-effort clip id extraction from an upload/create payload.
fn extract_id(body: &Value, what: &str) -> i64 {
    for v in [body, &body["clip"]] {
        if let Some(id) = v["id"].as_i64().or(v["clip_id"].as_i64()) {
            return id;
        }
        if let Some(s) = v["id"].as_str().or(v["clip_id"].as_str()) {
            if let Ok(id) = s.parse::<i64>() {
                return id;
            }
        }
    }
    panic!("no usable `{what}` id in response: {body:?}");
}

/// Public clip detail (`clips::show`) for counter assertions.
async fn get_clip(inst: &Instance, clip_id: i64) -> Value {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/v1/clips/{clip_id}", inst.addr))
        .send()
        .await
        .expect("clip get");
    assert!(
        resp.status().is_success(),
        "GET clip {clip_id} failed: {}",
        resp.status()
    );
    resp.json::<Value>().await.unwrap_or_else(|_| json!({}))
}

fn like_count_of(clip: &Value, clip_id: i64) -> i64 {
    count_field(clip, "like_count")
        .unwrap_or_else(|| panic!("clip {clip_id} payload has no like_count: {clip:?}"))
}

fn comment_count_of(clip: &Value, clip_id: i64) -> i64 {
    count_field(clip, "comment_count")
        .unwrap_or_else(|| panic!("clip {clip_id} payload has no comment_count: {clip:?}"))
}

/// Upload a generated fixture clip as `who`; returns `(clip_id, raw_response)`.
async fn upload_fixture(
    inst: &Instance,
    who: &(String, String),
    dir: &std::path::Path,
    fname: &str,
    caption: Option<&str>,
    hue: i32,
) -> (i64, Value) {
    let Some(path) = fixture_clip_720(dir, fname, hue) else {
        panic!("could not generate ffmpeg fixture clip");
    };
    let bytes = std::fs::read(&path).expect("fixture bytes");
    let (status, body) = upload_clip(&inst.addr, &who.0, &who.1, fname, &bytes, caption).await;
    assert!(status.is_success(), "upload failed ({status}): {body:?}");
    (extract_id(&body, "clip"), body)
}

fn actor_username(entry: &Value) -> Option<&str> {
    entry["author"]["username"]
        .as_str()
        .or_else(|| entry["account"]["username"].as_str())
}

// ── tests ───────────────────────────────────────────────────────────────────

/// alice follows bob → bob uploads "#sunset beach" → following feed shows it,
/// search finds it by tag text, and the hashtags table has `sunset` linked.
#[tokio::test]
async fn test_following_feed_roundtrip() {
    let Some(inst) = setup("feed").await else {
        return;
    };
    let alice = register_and_login(&inst.addr, "alice").await;
    let bob = register_and_login(&inst.addr, "bob").await;

    // alice follows bob by actor URI
    let bob_uri = format!("http://localhost:{}/users/bob", inst.port);
    let (status, body) = http_post_json(
        &inst.addr,
        "/api/v1/follows",
        json!({ "actor_uri": bob_uri }),
        Some(&alice.0),
        Some(&alice.1),
    )
    .await;
    assert!(status.is_success(), "follow failed ({status}): {body:?}");

    // bob uploads a tiny 720p fixture with hashtags in the caption
    let tmp = tempfile::tempdir().expect("tmpdir");
    let (clip_id, _) = match fixture_clip_720(tmp.path(), "sunset.mp4", 0) {
        Some(p) => {
            let bytes = std::fs::read(p).expect("fixture bytes");
            let (status, body) = upload_clip(
                &inst.addr,
                &bob.0,
                &bob.1,
                "sunset.mp4",
                &bytes,
                Some("#sunset beach"),
            )
            .await;
            assert!(
                status.is_success(),
                "bob upload failed ({status}): {body:?}"
            );
            (extract_id(&body, "clip"), body)
        }
        None => {
            eprintln!("ffmpeg unavailable; skipping test_following_feed_roundtrip");
            return;
        }
    };

    // wait for the transcode ladder to publish the clip into alice's feed
    let addr = inst.addr.clone();
    let ac = alice.clone();
    wait_until(
        Duration::from_secs(180),
        Duration::from_millis(3000),
        "clip ready in following feed",
        || {
            let addr = addr.clone();
            let ac = ac.clone();
            async move {
                let (status, body) =
                    http_get_authed(&addr, "/api/v1/feed/following", &ac.0, &ac.1).await;
                // 429s during polling are fine: back off and retry.
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    return Some("rate limited; will retry".into());
                }
                let ok = status.is_success()
                    && as_items(&body, &["items", "clips", "feed"])
                        .into_iter()
                        .any(|it| {
                            actor_username(it) == Some("bob")
                                && it["asset_url"].as_str().is_some_and(|u| !u.is_empty())
                        });
                if ok {
                    None
                } else {
                    Some(format!("{status} {}", snip(&body)))
                }
            }
        },
    )
    .await;

    // final feed assertions
    let (status, body) =
        http_get_authed(&inst.addr, "/api/v1/feed/following", &alice.0, &alice.1).await;
    assert!(
        status.is_success(),
        "following feed failed ({status}): {body:?}"
    );
    let entry = as_items(&body, &["items", "clips", "feed"])
        .into_iter()
        .find(|it| actor_username(it) == Some("bob"))
        .unwrap_or_else(|| panic!("bob's clip missing from following feed: {body:?}"));
    assert!(
        entry["asset_url"].as_str().is_some_and(|u| !u.is_empty()),
        "feed entry has empty asset_url: {entry:?}"
    );

    // search finds it by tag text
    let (status, body) = http_get_authed(
        &inst.addr,
        "/api/v1/search?q=sunset&type=clips",
        &alice.0,
        &alice.1,
    )
    .await;
    assert!(status.is_success(), "search failed ({status}): {body:?}");
    let hits = as_items(&body, &["clips", "items", "results"]);
    assert!(
        hits.iter().any(|it| it["id"].as_i64() == Some(clip_id)),
        "search q=sunset&type=clips did not return clip {clip_id}: {body:?}"
    );

    // hashtags table has row tag='sunset' linked to the clip
    let tagged: Option<i64> = sqlx::query_scalar(
        r#"
        SELECT h.id FROM hashtags h
        JOIN clip_hashtags ch ON ch.hashtag_id = h.id
        WHERE h.tag = 'sunset' AND ch.clip_id = $1
        "#,
    )
    .bind(clip_id)
    .fetch_optional(&inst.pool)
    .await
    .expect("hashtag join query");
    assert!(
        tagged.is_some(),
        "no hashtags row tag='sunset' linked to clip {clip_id}"
    );
}

/// like → 1; like again → still 1; unlike → 0; unlike again → still 0.
#[tokio::test]
async fn test_like_unlike_roundtrip() {
    let Some(inst) = setup("likes").await else {
        return;
    };
    let bob = register_and_login(&inst.addr, "bob").await;
    let alice = register_and_login(&inst.addr, "alice").await;

    let tmp = tempfile::tempdir().expect("tmpdir");
    let (clip_id, _) = upload_fixture(&inst, &bob, tmp.path(), "like-me.mp4", None, 0).await;

    let like_path = format!("/api/v1/clips/{clip_id}/like");

    let (status, body) = http_post_json(
        &inst.addr,
        &like_path,
        json!({}),
        Some(&alice.0),
        Some(&alice.1),
    )
    .await;
    assert!(
        status.is_success(),
        "first like failed ({status}): {body:?}"
    );
    assert_eq!(
        like_count_of(&get_clip(&inst, clip_id).await, clip_id),
        1,
        "like_count should be 1 after first like"
    );

    // idempotent re-like
    let (status, body) = http_post_json(
        &inst.addr,
        &like_path,
        json!({}),
        Some(&alice.0),
        Some(&alice.1),
    )
    .await;
    assert!(
        status.is_success(),
        "second like failed ({status}): {body:?}"
    );
    assert_eq!(
        like_count_of(&get_clip(&inst, clip_id).await, clip_id),
        1,
        "double like must stay at 1"
    );

    let (status, body) = http_delete(&inst.addr, &like_path, &alice.0, &alice.1).await;
    assert!(status.is_success(), "unlike failed ({status}): {body:?}");
    assert_eq!(
        like_count_of(&get_clip(&inst, clip_id).await, clip_id),
        0,
        "like_count should be 0 after unlike"
    );

    // idempotent re-unlike
    let (status, body) = http_delete(&inst.addr, &like_path, &alice.0, &alice.1).await;
    assert!(
        status.is_success(),
        "second unlike failed ({status}): {body:?}"
    );
    assert_eq!(
        like_count_of(&get_clip(&inst, clip_id).await, clip_id),
        0,
        "double unlike must stay at 0"
    );
}

/// comment → count 1 + bob gets a `comment` notification from alice;
/// mark-read empties unread; deleting the comment drops count to 0 and
/// tombstones the body to `[deleted]`.
#[tokio::test]
async fn test_comment_notification_delete() {
    let Some(inst) = setup("comments").await else {
        return;
    };
    let bob = register_and_login(&inst.addr, "bob").await;
    let alice = register_and_login(&inst.addr, "alice").await;

    let tmp = tempfile::tempdir().expect("tmpdir");
    let (clip_id, _) = upload_fixture(&inst, &bob, tmp.path(), "talk.mp4", None, 0).await;

    // alice comments
    let comments_path = format!("/api/v1/clips/{clip_id}/comments");
    let (status, body) = http_post_json(
        &inst.addr,
        &comments_path,
        json!({ "body": "nice #wave" }),
        Some(&alice.0),
        Some(&alice.1),
    )
    .await;
    assert!(status.is_success(), "comment failed ({status}): {body:?}");
    let comment_id = extract_id(&body, "comment");

    assert_eq!(
        comment_count_of(&get_clip(&inst, clip_id).await, clip_id),
        1,
        "comment_count should be 1 after alice commented"
    );

    // bob has a `comment` notification authored by alice
    let (status, body) = http_get_authed(&inst.addr, "/api/v1/notifications", &bob.0, &bob.1).await;
    assert!(
        status.is_success(),
        "notifications failed ({status}): {body:?}"
    );
    let notifs = as_items(&body, &["notifications", "items"]);
    let hit = notifs.iter().any(|n| {
        let kind = n["kind"].as_str().or_else(|| n["type"].as_str());
        kind == Some("comment")
            && (n["source"]["username"].as_str() == Some("alice")
                || n["actor"]["username"].as_str() == Some("alice")
                || n["from"]["username"].as_str() == Some("alice")
                || n["actor_username"].as_str() == Some("alice"))
    });
    assert!(hit, "no comment notification from alice: {body:?}");

    // mark-read empties unread
    let (status, body) = http_put_json(
        &inst.addr,
        "/api/v1/notifications/read",
        json!({}),
        &bob.0,
        &bob.1,
    )
    .await;
    assert!(status.is_success(), "mark-read failed ({status}): {body:?}");
    let (_, body) = http_get_authed(&inst.addr, "/api/v1/notifications", &bob.0, &bob.1).await;
    let unread_left = as_items(&body, &["notifications", "items"])
        .iter()
        .filter(|n| n["read"] == json!(false) || n["read_at"].is_null() && n["read"].is_null())
        .count();
    assert_eq!(
        unread_left, 0,
        "unread notifications remain after mark-read: {body:?}"
    );

    // alice deletes her own comment
    let (status, body) = http_delete(
        &inst.addr,
        &format!("/api/v1/comments/{comment_id}"),
        &alice.0,
        &alice.1,
    )
    .await;
    assert!(
        status.is_success(),
        "delete comment failed ({status}): {body:?}"
    );
    assert_eq!(
        comment_count_of(&get_clip(&inst, clip_id).await, clip_id),
        0,
        "comment_count should be back to 0 after deletion"
    );

    // tombstone body (deleted comments are filtered out of listings, so
    // verify the '[deleted]' marker on the row itself)
    let body_html: Option<String> =
        sqlx::query_scalar("SELECT body_html FROM comments WHERE id = $1")
            .bind(comment_id)
            .fetch_optional(&inst.pool)
            .await
            .expect("comment fetch");
    assert_eq!(
        body_html.as_deref(),
        Some("[deleted]"),
        "deleted comment not tombstoned as '[deleted]': {body_html:?}"
    );
}

/// search across actors / clips / tags types, including empty results that
/// must come back as (empty) arrays rather than errors.
#[tokio::test]
async fn test_search_actors_tags_clips() {
    let Some(inst) = setup("search").await else {
        return;
    };
    let tmp = tempfile::tempdir().expect("tmpdir");
    let carol = register_and_login(&inst.addr, "carol").await;
    register_and_login(&inst.addr, "dave").await;
    let (_id, _) = upload_fixture(
        &inst,
        &carol,
        tmp.path(),
        "hello.mp4",
        Some("hello world"),
        0,
    )
    .await;
    let clip_id_search = _id;

    // clip search only matches status='ready' clips — wait out the transcode
    wait_until(
        Duration::from_secs(180),
        Duration::from_millis(500),
        "carol's clip ready for search",
        || {
            let inst = &inst;
            async move {
                let clip = get_clip(inst, clip_id_search).await;
                if clip["status"].as_str() == Some("ready") {
                    None
                } else {
                    Some(snip(&clip))
                }
            }
        },
    )
    .await;

    // q=carol&type=actors hits carol
    let (status, body) = http_get_authed(
        &inst.addr,
        "/api/v1/search?q=carol&type=actors",
        &carol.0,
        &carol.1,
    )
    .await;
    assert!(
        status.is_success(),
        "actor search failed ({status}): {body:?}"
    );
    let hits = as_items(&body, &["actors", "items", "results"]);
    assert!(
        hits.iter().any(|a| a["username"].as_str() == Some("carol")
            || a["actor"]["username"].as_str() == Some("carol")),
        "q=carol&type=actors did not hit carol: {body:?}"
    );

    // q=hello&type=clips hits the uploaded clip
    let (status, body) = http_get_authed(
        &inst.addr,
        "/api/v1/search?q=hello&type=clips",
        &carol.0,
        &carol.1,
    )
    .await;
    assert!(
        status.is_success(),
        "clip search failed ({status}): {body:?}"
    );
    let hits = as_items(&body, &["clips", "items", "results"]);
    assert!(
        !hits.is_empty(),
        "q=hello&type=clips returned no hits: {body:?}"
    );

    // q=hx&type=tags → empty-ok (search requires q >= 2 chars, so a
    // two-char no-match probe stands in for the spec's `q=h`)
    let (status, body) = http_get_authed(
        &inst.addr,
        "/api/v1/search?q=hx&type=tags",
        &carol.0,
        &carol.1,
    )
    .await;
    assert!(
        status.is_success(),
        "tag search failed ({status}): {body:?}"
    );
    let hits = as_items(&body, &["tags", "items", "results"]);
    assert!(hits.is_empty(), "q=hx&type=tags should be empty: {body:?}");

    // untyped q=xx → all-empty result object, not an error
    let (status, body) =
        http_get_authed(&inst.addr, "/api/v1/search?q=xx", &carol.0, &carol.1).await;
    assert!(
        status.is_success(),
        "untyped search failed ({status}): {body:?}"
    );
    for k in ["actors", "tags", "clips"] {
        let arr = as_items(&body[k], &[]);
        assert!(
            arr.is_empty(),
            "search q=x should return empty {k}: {body:?}"
        );
    }
}

/// report a clip → visible to admin in open queue → admin resolves it.
#[tokio::test]
async fn test_report_admin_resolve() {
    let Some(inst) = setup("reports").await else {
        return;
    };
    let uploader = register_and_login(&inst.addr, "uploader").await;
    let reporter = register_and_login(&inst.addr, "reporter").await;

    let tmp = tempfile::tempdir().expect("tmpdir");
    let (clip_id, _) = upload_fixture(&inst, &uploader, tmp.path(), "bad.mp4", None, 0).await;

    let (status, body) = http_post_json(
        &inst.addr,
        "/api/v1/reports",
        json!({
            "target_type": "clip",
            "target_id": clip_id,
            "category": "spam",
            "body": "bad"
        }),
        Some(&reporter.0),
        Some(&reporter.1),
    )
    .await;
    assert!(
        status.is_success(),
        "report creation failed ({status}): {body:?}"
    );
    let report_id = extract_id(&body, "report");

    create_admin_directly(&inst.pool, inst.port).await;
    let admin = {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/api/v1/auth/login", inst.addr))
            .json(&json!({ "username_or_email": "admin", "password": "adminpassword123" }))
            .send()
            .await
            .expect("admin login");
        assert_eq!(resp.status(), reqwest::StatusCode::OK, "admin login");
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
        let body: Value = resp.json().await.expect("admin login body");
        (
            cookie,
            body["csrf_token"].as_str().expect("csrf_token").to_string(),
        )
    };

    // open queue shows the report
    let (status, body) = http_get_authed(
        &inst.addr,
        "/api/v1/admin/reports?state=open",
        &admin.0,
        &admin.1,
    )
    .await;
    assert!(
        status.is_success(),
        "admin reports failed ({status}): {body:?}"
    );
    let reports = as_items(&body, &["reports", "items"]);
    let found = reports.iter().find(|r| r["id"].as_i64() == Some(report_id));
    let found = found.unwrap_or_else(|| panic!("report {report_id} not in open queue: {body:?}"));
    assert_eq!(
        found["state"].as_str(),
        Some("open"),
        "reported state not open: {found:?}"
    );

    // resolve with an action note
    let (status, body) = http_post_json(
        &inst.addr,
        &format!("/api/v1/admin/reports/{report_id}/resolve"),
        json!({ "action_note": "handled" }),
        Some(&admin.0),
        Some(&admin.1),
    )
    .await;
    assert!(status.is_success(), "resolve failed ({status}): {body:?}");

    let (status, body) = http_get_authed(
        &inst.addr,
        "/api/v1/admin/reports?state=resolved",
        &admin.0,
        &admin.1,
    )
    .await;
    assert!(
        status.is_success(),
        "resolved listing failed ({status}): {body:?}"
    );
    let reports = as_items(&body, &["reports", "items"]);
    assert!(
        reports.iter().any(|r| r["id"].as_i64() == Some(report_id)),
        "report {report_id} not in resolved queue: {body:?}"
    );
}

/// bob uploads two clips → his profile grid shows both, keyed by his handle.
#[tokio::test]
async fn test_profile_grid() {
    let Some(inst) = setup("profiles").await else {
        return;
    };
    let bob = register_and_login(&inst.addr, "bob").await;

    let tmp = tempfile::tempdir().expect("tmpdir");
    let (_id1, _) = upload_fixture(&inst, &bob, tmp.path(), "one.mp4", None, 10).await;
    let (_id2, _) = upload_fixture(&inst, &bob, tmp.path(), "two.mp4", None, 20).await;

    let addr = inst.addr.clone();
    wait_until(
        Duration::from_secs(180),
        Duration::from_millis(500),
        "both clips ready on profile grid",
        || {
            let addr = addr.clone();
            async move {
                let client = reqwest::Client::new();
                let Ok(resp) = client
                    .get(format!("{addr}/api/v1/profiles/bob"))
                    .send()
                    .await
                else {
                    return Some("profile request failed".to_string());
                };
                let status = resp.status();
                let body = resp.json::<Value>().await.unwrap_or_else(|_| json!({}));
                if as_items(&body, &["clips", "items"]).len() >= 2 {
                    None
                } else {
                    Some(format!("{status} {}", snip(&body)))
                }
            }
        },
    )
    .await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/api/v1/profiles/bob", inst.addr))
        .send()
        .await
        .expect("profile get");
    assert!(
        resp.status().is_success(),
        "profile fetch failed: {}",
        resp.status()
    );
    let body: Value = resp.json::<Value>().await.expect("profile body");
    assert_eq!(
        body["actor"]["username"].as_str(),
        Some("bob"),
        "profile actor mismatch: {body:?}"
    );
    let clips = as_items(&body, &["clips", "items"]);
    assert_eq!(clips.len(), 2, "expected exactly 2 profile clips: {body:?}");
}
