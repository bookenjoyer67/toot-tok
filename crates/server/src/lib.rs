//! toottok — server library: config, router, auth/accounts/admin APIs,
//! upload pipeline, job runner, CLI.

pub mod accounts;
pub mod admin;
pub mod assets;
pub mod auth;
pub mod clips;
pub mod config;
pub mod csrf;
pub mod federation;
pub mod keys;
pub mod mail;
pub mod problem;
pub mod ratelimit;
pub mod session;
pub mod settings;
pub mod social;
pub mod upload;
pub mod worker;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::extract::Request;
use axum::middleware::{from_fn, Next};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use toottok_db::actor::Actor;
use toottok_db::user::User;
use toottok_federation::activitypub_federation::config::FederationConfig;
use toottok_federation::activitypub_federation::config::FederationMiddleware;
use toottok_federation::data::FederationData;
use toottok_federation::egress::EgressGuard;
use toottok_media::store::{LocalStore, Store};
use toottok_media::transcode;

use crate::config::Config;
use crate::mail::{LogMailer, Mailer};
use crate::ratelimit::RateLimiter;

/// Shared router state. `pool` is `None` only in `TOOTTOK_SKIP_DB` mode.
#[derive(Clone)]
pub struct AppState {
    pub pool: Option<sqlx::PgPool>,
    pub store: Arc<dyn Store>,
    pub cfg: Config,
    pub mailer: Arc<dyn Mailer>,
    pub rate_limit_auth: RateLimiter,
    pub rate_limit_upload: RateLimiter,
    pub rate_limit_accounts: RateLimiter,
    pub rate_limit_admin: RateLimiter,
    pub rate_limit_default: RateLimiter,
    /// ActivityPub federation config (present when a pool is configured and
    /// [`AppState::init_federation`] has been called). Its middleware provides
    /// `Data<FederationData>` to the AP routes.
    pub fed_config: Option<FederationConfig<FederationData>>,
    /// Egress guard for outbound federation (delivery + remote actor fetch).
    pub egress: EgressGuard,
}

impl AppState {
    /// Convenience constructor with production defaults (LogMailer, standard
    /// rate buckets). Used by `serve()`.
    pub fn new(pool: Option<sqlx::PgPool>, store: Arc<dyn Store>, cfg: Config) -> Self {
        let trusted = cfg.trusted_proxies.clone();
        Self {
            pool,
            store,
            cfg,
            mailer: Arc::new(LogMailer),
            rate_limit_auth: RateLimiter::auth().with_trusted_proxies(trusted.clone()),
            rate_limit_upload: RateLimiter::upload().with_trusted_proxies(trusted.clone()),
            rate_limit_accounts: RateLimiter::accounts().with_trusted_proxies(trusted.clone()),
            rate_limit_admin: RateLimiter::admin().with_trusted_proxies(trusted.clone()),
            rate_limit_default: RateLimiter::general().with_trusted_proxies(trusted),
            fed_config: None,
            egress: EgressGuard::new(false),
        }
    }

    /// Test/bench construction: generous rate limits so individual flows never
    /// trip buckets; override individual fields for rate-limit tests.
    pub fn test_default(pool: Option<sqlx::PgPool>, store: Arc<dyn Store>) -> Self {
        Self {
            pool,
            store,
            cfg: Config::default(),
            mailer: Arc::new(LogMailer),
            rate_limit_auth: RateLimiter::new(1000),
            rate_limit_upload: RateLimiter::new(1000),
            rate_limit_accounts: RateLimiter::new(1000),
            rate_limit_admin: RateLimiter::new(1000),
            rate_limit_default: RateLimiter::new(1000),
            fed_config: None,
            egress: EgressGuard::new(false),
        }
    }

    /// Ensure the instance actor exists (generating an RSA-2048 keypair on
    /// first boot), build the federation config, and configure the egress
    /// guard. No-op without a pool.
    pub async fn init_federation(&mut self) -> anyhow::Result<()> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };
        let base = self.cfg.public_base_url();
        let instance_ap = format!("{base}/ap/actor");

        let (public_key_pem, private_key_pem) =
            match Actor::fetch_by_ap_id(pool, &instance_ap).await {
                Ok(Some(existing)) => (
                    existing.public_key_pem.clone(),
                    existing.private_key_pem.clone().unwrap_or_default(),
                ),
                _ => keys::generate_actor_keypair()?,
            };

        Actor::ensure_instance_actor(
            pool,
            &self.cfg.domain,
            &public_key_pem,
            &private_key_pem,
            &instance_ap,
            &format!("{base}/ap/inbox"),
            &format!("{base}/ap/outbox"),
            &format!("{base}/ap/followers"),
        )
        .await?;

        self.egress = EgressGuard::new(self.cfg.allow_insecure_loopback_peers);
        let data = FederationData {
            pool: pool.clone(),
            domain: self.cfg.federation_domain(),
            base_url: base,
            allow_loopback: self.cfg.allow_insecure_loopback_peers,
        };
        self.fed_config = Some(toottok_federation::build_config(data).await?);
        Ok(())
    }

    /// The federation egress guard (constructed by [`AppState::init_federation`]
    /// or a test harness).
    pub fn egress(&self) -> &EgressGuard {
        &self.egress
    }
}

async fn healthz() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "toottok" }))
}

pub fn app(state: AppState) -> Router {
    let auth_limiter = state.rate_limit_auth.clone();
    let upload_limiter = state.rate_limit_upload.clone();
    let accounts_limiter = state.rate_limit_accounts.clone();
    let admin_limiter = state.rate_limit_admin.clone();
    let default_limiter = state.rate_limit_default.clone();
    let csrf_state = state.clone();

    // Route classes are isolated per group so auth/upload routes only consume
    // their own buckets, and the CSRF check wraps the whole API.
    let auth_routes = Router::new()
        .route("/api/v1/auth/register", post(auth::register))
        .route("/api/v1/auth/login", post(auth::login))
        .route("/api/v1/auth/logout", post(auth::logout))
        .route("/api/v1/auth/request-reset", post(auth::request_reset))
        .route("/api/v1/auth/reset", post(auth::reset))
        .route("/api/v1/auth/verify-email", post(auth::verify_email))
        .route_layer(from_fn(move |req: Request, next: Next| {
            let limiter = auth_limiter.clone();
            async move { crate::ratelimit::apply(limiter, req, next).await }
        }));

    let upload_routes = Router::new()
        .route(
            "/api/v1/clips/upload",
            // Uploads are size-capped mid-stream inside the handler (413
            // problem+json on over-cap), so disable axum's 2MB DefaultBodyLimit.
            post(upload::upload).layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route_layer(from_fn(move |req: Request, next: Next| {
            let limiter = upload_limiter.clone();
            async move { crate::ratelimit::apply(limiter, req, next).await }
        }));

    let account_routes = Router::new()
        .route(
            "/api/v1/accounts/me",
            get(accounts::me)
                .patch(accounts::patch_me)
                .delete(accounts::delete_me),
        )
        .route("/api/v1/accounts/me/avatar", post(accounts::avatar))
        .route("/api/v1/feed/following", get(social::following_feed))
        .route("/api/v1/feed/discover", get(social::discover_feed))
        .route("/api/v1/feed/local", get(social::local_feed))
        .route("/api/v1/feed/trending", get(social::trending_feed))
        .route("/api/v1/tags/trending", get(social::trending_tags))
        .route("/api/v1/sounds/{id}", get(social::sound_detail))
        .route("/api/v1/sounds/{id}/clips", get(social::sound_clips))
        .route(
            "/api/v1/clips/{id}/like",
            post(social::like_clip).delete(social::unlike_clip),
        )
        .route(
            "/api/v1/clips/{id}/announce",
            post(social::announce_clip).delete(social::unannounce_clip),
        )
        .route(
            "/api/v1/clips/{id}/bookmark",
            put(social::bookmark_clip).delete(social::unbookmark_clip),
        )
        .route("/api/v1/bookmarks", get(social::list_bookmarks))
        .route(
            "/api/v1/comments/{id}",
            axum::routing::delete(social::delete_comment),
        )
        .route("/api/v1/notifications", get(social::list_notifications))
        .route("/api/v1/notifications/read", put(social::mark_read))
        .route("/api/v1/reports", post(social::create_report))
        .route("/api/v1/clips/{id}/comments", post(social::create_comment))
        .route_layer(from_fn(move |req: Request, next: Next| {
            let limiter = accounts_limiter.clone();
            async move { crate::ratelimit::apply(limiter, req, next).await }
        }));

    let admin_routes = Router::new()
        .route("/api/v1/admin/users", get(admin::list_users))
        .route(
            "/api/v1/admin/users/{id}/approve",
            post(admin::approve_user),
        )
        .route(
            "/api/v1/admin/users/{id}/suspend",
            post(admin::suspend_user),
        )
        .route(
            "/api/v1/admin/domain-blocks",
            post(admin::create_domain_block),
        )
        .route(
            "/api/v1/admin/domain-blocks/{domain}",
            axum::routing::delete(admin::delete_domain_block),
        )
        .route(
            "/api/v1/admin/settings",
            get(admin::get_settings).put(admin::put_settings),
        )
        .route("/api/v1/admin/reports", get(social::admin_reports))
        .route(
            "/api/v1/admin/reports/{id}/resolve",
            post(social::resolve_report),
        )
        .route_layer(from_fn(move |req: Request, next: Next| {
            let limiter = admin_limiter.clone();
            async move { crate::ratelimit::apply(limiter, req, next).await }
        }));

    let default_routes = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/v1/clips/{id}", get(clips::show))
        .route("/api/v1/clips/{id}/comments", get(social::list_comments))
        .route("/api/v1/search", get(social::search))
        .route("/api/v1/tags/{tag}/clips", get(social::tag_clips))
        .route("/api/v1/profiles/{username}", get(social::profile_grid))
        .route(
            "/assets/{clip_id}/{filename}",
            get(assets::asset).head(assets::asset),
        )
        .route_layer(from_fn(move |req: Request, next: Next| {
            let limiter = default_limiter.clone();
            async move { crate::ratelimit::apply(limiter, req, next).await }
        }));

    let mut router = Router::new()
        .merge(auth_routes)
        .merge(upload_routes)
        .merge(account_routes)
        .merge(admin_routes)
        .merge(default_routes);

    if let Some(config) = state.fed_config.clone() {
        router = router.merge(federation_routes(config));
    }

    // Serve the built SvelteKit frontend at `/` with an index.html fallback
    // so client-side routes (login, profile/…, admin, …) work on refresh.
    // API/asset routes above take precedence; unknown /api paths also get the
    // SPA shell, which is harmless for v1 (handlers 404 for real API misses).
    let web_dir = std::path::PathBuf::from(&state.cfg.web_dir);
    if web_dir.join("index.html").is_file() {
        let index = tower_http::services::ServeFile::new(web_dir.join("index.html"));
        let static_svc = tower_http::services::ServeDir::new(&web_dir).fallback(index);
        router = router.fallback_service(static_svc);
    }

    router
        .route_layer(axum::middleware::from_fn_with_state(
            csrf_state,
            csrf::csrf_middleware,
        ))
        .with_state(state)
}

/// ActivityPub server-to-server + outbound-follow routes, wrapped in the
/// federation middleware so handlers can extract `Data<FederationData>`.
fn federation_routes(config: FederationConfig<FederationData>) -> Router<AppState> {
    let default_limiter = RateLimiter::general();
    Router::new()
        .route("/.well-known/webfinger", get(federation::webfinger))
        .route("/.well-known/nodeinfo", get(federation::nodeinfo_jrd))
        .route("/nodeinfo/{version}", get(federation::nodeinfo_doc))
        .route("/ap/actor", get(federation::instance_actor))
        .route("/ap/inbox", post(federation::inbox))
        .route("/users/{username}", get(federation::user_actor))
        .route("/users/{username}/inbox", post(federation::inbox))
        .route(
            "/users/{username}/{collection}",
            get(federation::user_collection),
        )
        .route("/clips/{id}", get(federation::clip_object))
        .route("/clips/{id}/activity", get(federation::clip_activity))
        .route("/api/v1/follows", post(federation::api_follow))
        .route("/api/v1/follows/mine", get(federation::api_my_follows))
        .route(
            "/api/v1/follows/{target_id}/unfollow",
            post(federation::api_unfollow),
        )
        .route(
            "/api/v1/profiles/{username}/follow-state",
            get(federation::api_follow_state),
        )
        .route(
            "/api/v1/profiles/{username}/{list}",
            get(federation::api_follow_list),
        )
        .layer(FederationMiddleware::new(config))
        .route_layer(from_fn(move |req: Request, next: Next| {
            let limiter = default_limiter.clone();
            async move { crate::ratelimit::apply(limiter, req, next).await }
        }))
}

pub async fn main_entry() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "toottok=info,tower_http=info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("create-admin") => {
            let username = args
                .get(2)
                .context("usage: toottok create-admin <username> <password>")?;
            let password = args
                .get(3)
                .context("usage: toottok create-admin <username> <password>")?;
            create_admin(username, password).await?;
        }
        _ => serve().await?,
    }
    Ok(())
}

/// `toottok serve` (or bare `toottok`) starts the HTTP server. Migrates,
/// brings the worker pool up, then serves.
pub async fn serve() -> anyhow::Result<()> {
    let cfg = config::Config::load()?;
    let use_db = std::env::var("TOOTTOK_SKIP_DB").is_err();
    let pool = if use_db {
        Some(toottok_db::connect(&cfg.database_url).await?)
    } else {
        None
    };
    if let Some(pool) = &pool {
        toottok_db::migrate(pool).await?;
        tracing::info!("migrations applied");
    }

    let store: Arc<dyn Store> = Arc::new(LocalStore::new(&cfg.media_dir));
    transcode::set_threads(cfg.ffmpeg_threads);

    let mut state = AppState::new(pool, store, cfg);
    state.init_federation().await?;
    if let Some(pool) = &state.pool {
        let egress = state.egress.clone();
        worker::spawn_worker_pool(
            pool.clone(),
            state.store.clone(),
            state.cfg.worker_concurrency,
            Duration::from_secs(state.cfg.jobs_job_timeout_secs),
            egress.clone(),
            state.cfg.public_base_url(),
        )
        .await;
        worker::spawn_maintenance(
            pool.clone(),
            std::path::PathBuf::from(&state.cfg.media_dir),
            state.cfg.jobs_job_timeout_secs,
        )
        .await;
        worker::spawn_delivery_worker(pool.clone(), egress).await;
    }
    let addr = state
        .cfg
        .bind_addr
        .parse::<std::net::SocketAddr>()
        .context("bind_addr is not a valid SocketAddr")?;
    if !addr.ip().is_loopback() && !state.cfg.behind_tls {
        tracing::warn!(
            bind_addr = %addr,
            "serving on a non-loopback address without behind_tls: session cookies lack the Secure flag"
        );
    }
    let app = app(state);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("toottok listening on http://{addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// `toottok create-admin <username> <password>` creates a local admin actor
/// (with a real RSA-2048 keypair) plus a user flagged `is_admin`.
pub async fn create_admin(username: &str, password: &str) -> anyhow::Result<()> {
    let cfg = config::Config::load()?;
    let pool = toottok_db::connect(&cfg.database_url).await?;
    toottok_db::migrate(&pool).await?;

    let (public_key_pem, private_key_pem) = keys::generate_actor_keypair()?;
    let base = cfg.public_base_url();
    let actor = Actor::create(
        &pool,
        username,
        None,
        "person",
        &public_key_pem,
        Some(&private_key_pem),
        &format!("{base}/users/{username}/inbox"),
        Some(&format!("{base}/ap/inbox")),
        &format!("{base}/users/{username}/outbox"),
        &format!("{base}/users/{username}/followers"),
        &format!("{base}/users/{username}"),
    )
    .await?;

    let hashed = toottok_db::password::hash_password(password)
        .map_err(|e| anyhow::anyhow!("password hashing failed: {e}"))?;
    let user = User::create_admin(&pool, actor.id, None, &hashed).await?;
    println!(
        "created admin @{} (actor id {}, user id {}, is_admin {})",
        username, actor.id, user.id, user.is_admin
    );
    Ok(())
}

/// Test-support helpers shared by integration-test binaries (register+login a
/// fresh account, returning the session cookie and CSRF token).
pub mod testutil {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use serde_json::json;
    use tower::ServiceExt;

    /// Register `username` (password ≥10 chars) then log in, returning
    /// `(toottok_session cookie value, csrf_token)`. `None` on any non-success.
    pub async fn register_and_login(
        app: &Router,
        username: &str,
        password: &str,
    ) -> Option<(String, String)> {
        let reg = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/register")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "username": username,
                            "password": password,
                        }))
                        .ok()?,
                    ))
                    .ok()?,
            )
            .await
            .ok()?;
        if reg.status() != StatusCode::CREATED {
            return None;
        }

        let login = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({
                            "username_or_email": username,
                            "password": password,
                        }))
                        .ok()?,
                    ))
                    .ok()?,
            )
            .await
            .ok()?;
        if login.status() != StatusCode::OK {
            return None;
        }
        let headers = login.headers().clone();
        let cookie = headers
            .get("set-cookie")?
            .to_str()
            .ok()?
            .split(';')
            .next()?
            .to_string();
        let body = axum::body::to_bytes(login.into_body(), 1 << 20)
            .await
            .ok()?;
        let csrf = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()?
            .get("csrf_token")?
            .as_str()?
            .to_string();
        Some((cookie, csrf))
    }
}
