//! Phase 4 integration tests against a real Postgres — auth, sessions, CSRF,
//! rate limiting, admin approvals, email tokens, account deletion, and upload
//! attribution. Same harness conventions as the db/media test suites:
//! `TOOTTOK_TEST_DB` connection params, per-pid database, panic-loud unless
//! `TOOTTOK_TEST_SKIP=1`.

use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use toottok::mail::Mailer;
use toottok::ratelimit::RateLimiter;
use tower::ServiceExt;

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
                eprintln!(
                    "toottok-server test setup failed ({e}); TOOTTOK_TEST_SKIP=1 set, skipping"
                );
                None
            } else {
                panic!("toottok-server test setup failed: {e}");
            }
        }
    }
}

async fn setup_inner() -> Result<sqlx::PgPool, Box<dyn std::error::Error>> {
    let url = std::env::var("TOOTTOK_TEST_DB").unwrap_or_else(|_| DEFAULT_TEST_URL.to_string());
    let options: PgConnectOptions = url.parse()?;
    let db_name = format!("toottok_server_test_{}", std::process::id());

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

/// In-memory Mailer that records every message so tests can read the mailed
/// token (LogMailer would write it to the tracing output instead).
#[derive(Clone, Default)]
struct VecMailer {
    msgs: Arc<Mutex<Vec<(String, String, String)>>>, // (to, subject, body)
}

impl Mailer for VecMailer {
    fn send(&self, to: &str, subject: &str, body: &str) {
        self.msgs.lock().expect("mailer mutex").push((
            to.to_string(),
            subject.to_string(),
            body.to_string(),
        ));
    }
}

/// A test AppState: fresh tempdir media store, LogMailer swapped for a
/// recording mailer, and roomy rate buckets (override per-test as needed).
fn test_state(pool: sqlx::PgPool) -> AppStateForTest {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store: Arc<dyn toottok_media::store::Store> = Arc::new(
        toottok_media::store::LocalStore::new(tmp.path().join("media")),
    );
    AppStateForTest {
        inner: toottok::AppState::test_default(Some(pool), store),
        tmp,
    }
}

/// Wrapper so the tempdir (media root) lives as long as the router.
struct AppStateForTest {
    inner: toottok::AppState,
    #[allow(dead_code)]
    tmp: tempfile::TempDir,
}

fn app_for(state: AppStateForTest) -> (Router, VecMailer) {
    let mailer = VecMailer::default();
    let mut s = state.inner;
    s.mailer = Arc::new(mailer.clone());
    (toottok::app(s), mailer)
}

/// Perform one request against the router, returning status, headers, body.
async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(String, String)],
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let req = match body {
        Some(v) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            req.body(Body::from(serde_json::to_vec(&v).expect("json body")))
                .expect("valid request")
        }
        None => req.body(Body::empty()).expect("valid request"),
    };
    let resp = app.clone().oneshot(req).await.expect("oneshot");
    let status = resp.status();
    let headers = resp.headers().clone();
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("response body")
        .to_vec();
    (status, headers, body)
}

fn body_json(body: &[u8]) -> Value {
    serde_json::from_slice(body).expect("response body is json")
}

fn cookie_from(headers: &HeaderMap) -> String {
    headers
        .get(header::SET_COOKIE)
        .expect("set-cookie header")
        .to_str()
        .expect("set-cookie is valid")
        .split(';')
        .next()
        .expect("cookie segment")
        .to_string()
}

fn csrf_from(body: &[u8]) -> String {
    body_json(body)["csrf_token"]
        .as_str()
        .expect("csrf_token in body")
        .to_string()
}

/// Register `username` then log in; returns the session cookie + csrf token.
async fn register_and_login(app: &Router, username: &str) -> (String, String) {
    let (status, _, _) = send(
        app,
        "POST",
        "/api/v1/auth/register",
        &[],
        Some(json!({ "username": username, "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "register {username}");
    login_only(app, username, "password123").await
}

/// Log in an existing account (created directly in the DB, not via register).
async fn login_only(app: &Router, ident: &str, password: &str) -> (String, String) {
    let (status, headers, body) = send(
        app,
        "POST",
        "/api/v1/auth/login",
        &[],
        Some(json!({ "username_or_email": ident, "password": password })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "login {ident}");
    (cookie_from(&headers), csrf_from(&body))
}

fn auth_headers(cookie: &str, csrf: &str) -> Vec<(String, String)> {
    vec![
        ("cookie".to_string(), cookie.to_string()),
        ("x-toottok-csrf".to_string(), csrf.to_string()),
    ]
}

fn cookie_header(cookie: &str) -> Vec<(String, String)> {
    vec![("cookie".to_string(), cookie.to_string())]
}

const DEDUP_FIXTURE: &[u8] = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isomiso2mp41";

fn multipart_body(boundary: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"clip.mp4\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: video/mp4\r\n\r\n");
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// Full cycle: register → login → me → patch → logout → 401 on me.
#[tokio::test]
async fn full_cycle_register_login_me_patch_logout() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    let (cookie, csrf) = register_and_login(&app, "alice").await;

    let (status, _, body) = send(
        &app,
        "GET",
        "/api/v1/accounts/me",
        &cookie_header(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let me = body_json(&body);
    assert_eq!(me["username"], "alice");
    assert_eq!(me["status"], "active");
    assert_eq!(me["csrf_token"], csrf, "me echoes the session csrf");

    let (status, _, body) = send(
        &app,
        "PATCH",
        "/api/v1/accounts/me",
        &auth_headers(&cookie, &csrf),
        Some(json!({ "display_name": "Alice A", "summary": "hello" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let patched = body_json(&body);
    assert_eq!(patched["display_name"], "Alice A");
    assert_eq!(patched["summary"], "hello");

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/logout",
        &auth_headers(&cookie, &csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = send(
        &app,
        "GET",
        "/api/v1/accounts/me",
        &cookie_header(&cookie),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "session gone after logout"
    );
}

/// Duplicate username → 409 problem+json.
#[tokio::test]
async fn duplicate_username_409() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        &[],
        Some(json!({ "username": "bob", "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, headers, _) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        &[],
        Some(json!({ "username": "bob", "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        headers[header::CONTENT_TYPE].to_str().unwrap(),
        "application/problem+json"
    );
}

/// Validation gates: uppercase username and short password are both 400.
#[tokio::test]
async fn register_validation_400s() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        &[],
        Some(json!({ "username": "Alice", "password": "password123" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "uppercase username rejected"
    );

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        &[],
        Some(json!({ "username": "carol", "password": "short" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "short password rejected");
}

/// Email gate (P3c): register and request-reset both reject addresses that do
/// not look like `local@domain.tld`.
#[tokio::test]
async fn email_validation_rejects_bad_addresses() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    for bad in ["not-an-email", "a@b", "a@b.", "a@.com", "a b@example.com"] {
        let (status, _, _) = send(
            &app,
            "POST",
            "/api/v1/auth/register",
            &[],
            Some(json!({ "username": "carol", "password": "password123", "email": bad })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "register rejects email {bad:?}"
        );
    }

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/request-reset",
        &[],
        Some(json!({ "email": "not-an-email" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "request-reset rejects malformed email"
    );

    // A well-formed address still answers 202 (no enumeration).
    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/request-reset",
        &[],
        Some(json!({ "email": "ghost@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

/// Wrong password on login → 401.
#[tokio::test]
async fn wrong_password_401() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        &[],
        Some(json!({ "username": "dave", "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        &[],
        Some(json!({ "username_or_email": "dave", "password": "wrongpassword" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Rate limiter: after the (tiny) bucket is exhausted the next attempt is 429
/// with Retry-After, even for otherwise-valid requests.
#[tokio::test]
async fn rate_limit_triggers_429() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let mut state = state.inner;
    state.rate_limit_auth = RateLimiter::new(3);
    let app = toottok::app(state);

    for _ in 0..3 {
        let (status, _, _) = send(
            &app,
            "POST",
            "/api/v1/auth/login",
            &[],
            Some(json!({ "username_or_email": "nobody", "password": "password123" })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "under-limit attempt is handled"
        );
    }

    let (status, headers, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        &[],
        Some(json!({ "username_or_email": "nobody", "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "bucket exhausted");
    assert!(headers.contains_key(header::RETRY_AFTER));
    assert_eq!(
        headers[header::CONTENT_TYPE].to_str().unwrap(),
        "application/problem+json"
    );
}

/// CSRF: a cookie-authenticated PATCH without the header is 403; with it, 200.
#[tokio::test]
async fn csrf_missing_header_403() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    let (cookie, csrf) = register_and_login(&app, "erin").await;

    let (status, _, _) = send(
        &app,
        "PATCH",
        "/api/v1/accounts/me",
        &cookie_header(&cookie),
        Some(json!({ "display_name": "No CSRF" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "missing header rejected");

    let (status, _, _) = send(
        &app,
        "PATCH",
        "/api/v1/accounts/me",
        &auth_headers(&cookie, &csrf),
        Some(json!({ "display_name": "With CSRF" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "matching header accepted");
}

/// Admin approval flow: approval-mode signup is pending (login 403) until an
/// admin approves it (then login succeeds). Every admin write lands in audit_log.
#[tokio::test]
async fn admin_approve_flow_makes_pending_user_active() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    toottok_db::settings::Setting::set(&pool, "registration_mode", &json!("approval"))
        .await
        .expect("set approval mode");

    // Create an admin actor + user directly (real keypair, like the CLI).
    let (pub_pem, priv_pem) = toottok::keys::generate_actor_keypair().expect("keypair");
    let admin_ap = "https://toottok.local/users/admin";
    let admin_actor = toottok_db::actor::Actor::create(
        &pool,
        "admin",
        None,
        "person",
        &pub_pem,
        Some(&priv_pem),
        &format!("{admin_ap}/inbox"),
        Some("https://toottok.local/ap/inbox"),
        &format!("{admin_ap}/outbox"),
        &format!("{admin_ap}/followers"),
        admin_ap,
    )
    .await
    .expect("admin actor");
    toottok_db::user::User::create_admin(
        &pool,
        admin_actor.id,
        None,
        &toottok_db::password::hash_password("adminpassword123").expect("hash"),
    )
    .await
    .expect("admin user");

    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    // Approval-mode signup: pending.
    let (status, _, body) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        &[],
        Some(json!({ "username": "frank", "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body_json(&body)["status"], "pending");

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        &[],
        Some(json!({ "username_or_email": "frank", "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "pending login refused");

    let frank_user_id: i64 = sqlx::query_scalar(
        "SELECT u.id FROM users u JOIN actors a ON a.id = u.actor_id WHERE a.username = 'frank'",
    )
    .fetch_one(&pool)
    .await
    .expect("frank user id");

    // Admin logs in and sees frank in the pending list.
    let (admin_cookie, admin_csrf) = login_only(&app, "admin", "adminpassword123").await;
    let (status, _, body) = send(
        &app,
        "GET",
        "/api/v1/admin/users?state=pending",
        &cookie_header(&admin_cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let users = body_json(&body)["users"]
        .as_array()
        .expect("users array")
        .clone();
    assert!(
        users
            .iter()
            .any(|u| u["user_id"] == json!(frank_user_id) && u["status"] == "pending"),
        "pending list contains frank"
    );

    // Approve → frank is active and can log in.
    let (status, _, _) = send(
        &app,
        "POST",
        &format!("/api/v1/admin/users/{frank_user_id}/approve"),
        &auth_headers(&admin_cookie, &admin_csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        &[],
        Some(json!({ "username_or_email": "frank", "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "approved user logs in");

    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'user.approve' AND target_id = $1",
    )
    .bind(frank_user_id)
    .fetch_one(&pool)
    .await
    .expect("audit count");
    assert_eq!(audit_count, 1, "approval is audit-logged");
}

/// Suspension (F1): a suspended account's live sessions are rejected (me →
/// 401), the sessions are revoked server-side, and a fresh login is refused.
#[tokio::test]
async fn suspend_user_kills_sessions_and_blocks_login() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };

    // Create an admin actor + user directly (real keypair, like the CLI).
    let (pub_pem, priv_pem) = toottok::keys::generate_actor_keypair().expect("keypair");
    let admin_ap = "https://toottok.local/users/admin";
    let admin_actor = toottok_db::actor::Actor::create(
        &pool,
        "admin",
        None,
        "person",
        &pub_pem,
        Some(&priv_pem),
        &format!("{admin_ap}/inbox"),
        Some("https://toottok.local/ap/inbox"),
        &format!("{admin_ap}/outbox"),
        &format!("{admin_ap}/followers"),
        admin_ap,
    )
    .await
    .expect("admin actor");
    toottok_db::user::User::create_admin(
        &pool,
        admin_actor.id,
        None,
        &toottok_db::password::hash_password("adminpassword123").expect("hash"),
    )
    .await
    .expect("admin user");

    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    // Register + login + me all work while active.
    let (cookie, _csrf) = register_and_login(&app, "mike").await;
    let (status, _, _) = send(
        &app,
        "GET",
        "/api/v1/accounts/me",
        &cookie_header(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "active account serves /me");

    let mike_user_id: i64 = sqlx::query_scalar(
        "SELECT u.id FROM users u JOIN actors a ON a.id = u.actor_id WHERE a.username = 'mike'",
    )
    .fetch_one(&pool)
    .await
    .expect("mike user id");

    // Admin suspends mike.
    let (admin_cookie, admin_csrf) = login_only(&app, "admin", "adminpassword123").await;
    let (status, _, _) = send(
        &app,
        "POST",
        &format!("/api/v1/admin/users/{mike_user_id}/suspend"),
        &auth_headers(&admin_cookie, &admin_csrf),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin suspends mike");

    // Existing session is now rejected by the AuthUser extractor (401) even
    // though the cookie itself is still valid.
    let (status, _, _) = send(
        &app,
        "GET",
        "/api/v1/accounts/me",
        &cookie_header(&cookie),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "suspended account's session rejected"
    );

    // Session rows were revoked server-side (admin.rs must delete_for_user).
    let session_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = $1")
        .bind(mike_user_id)
        .fetch_one(&pool)
        .await
        .expect("session count");
    assert_eq!(
        session_count, 0,
        "all of mike's sessions revoked on suspend"
    );

    // Fresh login refused (account suspended).
    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        &[],
        Some(json!({ "username_or_email": "mike", "password": "password123" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "suspended account cannot log in"
    );

    // The suspend was audit-logged.
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE action = 'user.suspend' AND target_id = $1",
    )
    .bind(mike_user_id)
    .fetch_one(&pool)
    .await
    .expect("audit count");
    assert_eq!(audit_count, 1, "suspension is audit-logged");
}

/// Non-admin hitting the admin API → 403.
#[tokio::test]
async fn admin_api_requires_admin() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    let (cookie, csrf) = register_and_login(&app, "grace").await;
    let (status, _, _) = send(
        &app,
        "GET",
        "/api/v1/admin/users?state=all",
        &cookie_header(&cookie),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = send(
        &app,
        "PUT",
        "/api/v1/admin/settings",
        &auth_headers(&cookie, &csrf),
        Some(json!({ "settings": { "registration_mode": "invite" } })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// request-reset is always 202 (no enumeration); reset with the mailed token
/// changes the password — the old one stops working, the new one works.
#[tokio::test]
async fn request_reset_then_reset_changes_password() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, mailer) = app_for(state);

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        &[],
        Some(
            json!({ "username": "henry", "password": "password123", "email": "henry@example.com" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Unknown email still answers 202 (no enumeration).
    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/request-reset",
        &[],
        Some(json!({ "email": "ghost@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/request-reset",
        &[],
        Some(json!({ "email": "henry@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let token = {
        let msgs = mailer.msgs.lock().expect("mailer mutex");
        let reset = msgs
            .iter()
            .find(|(_, subject, _)| subject.contains("reset"))
            .expect("a reset email was mailed");
        extract_code(&reset.2).expect("reset token in mail body")
    };

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/reset",
        &[],
        Some(json!({ "token": token, "new_password": "brandnewpassword123" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        &[],
        Some(json!({ "username_or_email": "henry", "password": "password123" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "old password no longer works"
    );

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/login",
        &[],
        Some(json!({ "username_or_email": "henry", "password": "brandnewpassword123" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "new password works");
}

/// verify-email consumes the mailed verify token and stamps email_verified_at.
#[tokio::test]
async fn verify_email_sets_email_verified_at() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, mailer) = app_for(state);

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        &[],
        Some(json!({ "username": "iris", "password": "password123", "email": "iris@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let token = {
        let msgs = mailer.msgs.lock().expect("mailer mutex");
        let verify = msgs
            .iter()
            .find(|(_, subject, _)| subject.contains("verification"))
            .expect("a verify email was mailed");
        extract_code(&verify.2).expect("verify token in mail body")
    };

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/verify-email",
        &[],
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let verified: bool = sqlx::query_scalar(
        "SELECT (email_verified_at IS NOT NULL) FROM users u JOIN actors a ON a.id = u.actor_id WHERE a.username = 'iris'",
    )
    .fetch_one(&pool)
    .await
    .expect("verified flag");
    assert!(verified, "email_verified_at stamped");

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/verify-email",
        &[],
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "token is single-use");
}

/// Account deletion: sessions die (me → 401), clips flip to deleted, the email
/// is nulled, and the actor is tombstoned.
#[tokio::test]
async fn account_deletion_local_half() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    let (status, _, _) = send(
        &app,
        "POST",
        "/api/v1/auth/register",
        &[],
        Some(
            json!({ "username": "julie", "password": "password123", "email": "julie@example.com" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (cookie, csrf) = login_only(&app, "julie", "password123").await;

    let actor_id: i64 = sqlx::query_scalar("SELECT id FROM actors WHERE username = 'julie'")
        .fetch_one(&pool)
        .await
        .expect("actor id");
    toottok_db::clip::Clip::create_local(
        &pool,
        actor_id,
        "https://toot.local/clips/julie-delete",
        Some("<p>bye</p>"),
        "public",
        "ready",
        None,
    )
    .await
    .expect("clip insert");
    let clip_id: i64 = sqlx::query_scalar(
        "SELECT id FROM clips WHERE ap_id = 'https://toot.local/clips/julie-delete'",
    )
    .fetch_one(&pool)
    .await
    .expect("clip id");

    let (status, _, _) = send(
        &app,
        "DELETE",
        "/api/v1/accounts/me",
        &auth_headers(&cookie, &csrf),
        Some(json!({ "password": "password123" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = send(
        &app,
        "GET",
        "/api/v1/accounts/me",
        &cookie_header(&cookie),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "session rejected after deletion"
    );

    let clip = toottok_db::clip::Clip::fetch_by_id(&pool, clip_id)
        .await
        .expect("fetch clip")
        .expect("clip exists");
    assert!(clip.deleted_at.is_some(), "clip flipped to deleted");
    assert_eq!(clip.status, "deleted");

    let email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE actor_id = $1")
        .bind(actor_id)
        .fetch_one(&pool)
        .await
        .expect("email");
    assert!(email.is_none(), "email nulled for erasure");

    let user_deleted: bool =
        sqlx::query_scalar("SELECT (deleted_at IS NOT NULL) FROM users WHERE actor_id = $1")
            .bind(actor_id)
            .fetch_one(&pool)
            .await
            .expect("user deleted flag");
    assert!(user_deleted, "user marked deleted");

    let tombstoned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM tombstones WHERE ap_id = 'http://toottok.test/users/julie')",
    )
    .fetch_one(&pool)
    .await
    .expect("tombstone");
    assert!(tombstoned, "actor tombstoned");
}

/// Authed upload sets clips.actor_id to that actor.
#[tokio::test]
async fn upload_attribution_uses_authed_actor() {
    let _guard = test_lock().lock().await;
    let Some(pool) = setup().await else {
        return;
    };
    let state = test_state(pool.clone());
    let (app, _mailer) = app_for(state);

    let (cookie, csrf) = register_and_login(&app, "kate").await;

    let boundary = "toottok-attrib";
    let body = multipart_body(boundary, DEDUP_FIXTURE);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/clips/upload")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .header(header::COOKIE, &cookie)
                .header("x-toottok-csrf", &csrf)
                .body(Body::from(body))
                .expect("valid request"),
        )
        .await
        .expect("upload");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body");
    let clip_id = body_json(&bytes)["clip_id"].as_i64().expect("clip_id");

    let actor_id: i64 = sqlx::query_scalar("SELECT id FROM actors WHERE username = 'kate'")
        .fetch_one(&pool)
        .await
        .expect("actor id");
    let clip_actor: i64 = sqlx::query_scalar("SELECT actor_id FROM clips WHERE id = $1")
        .bind(clip_id)
        .fetch_one(&pool)
        .await
        .expect("clip actor");
    assert_eq!(clip_actor, actor_id, "clip attributed to the authed actor");
}

/// Extract the mailed code: the body is `TootTok {kind} code: {token}\n...`.
fn extract_code(body: &str) -> Option<String> {
    let marker = "code: ";
    let start = body.find(marker)? + marker.len();
    let rest = &body[start..];
    let end = rest.find('\n').unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}
