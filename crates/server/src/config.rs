//! Config loading: toottok.toml > env > defaults.

use std::net::IpAddr;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_bind")]
    pub bind_addr: String,
    #[serde(default = "default_db")]
    pub database_url: String,
    #[serde(default = "default_media_dir")]
    pub media_dir: String,
    /// Directory containing the built frontend (SvelteKit adapter-static
    /// output). Served at `/` with an index.html fallback for client-side
    /// routing. Default `./web/build`.
    #[serde(default = "default_web_dir")]
    pub web_dir: String,
    #[serde(default = "default_worker_concurrency")]
    pub worker_concurrency: usize,
    /// Per-job hard cap; a job that overruns it is treated as failed
    /// (clip failed, attempts bumped, possibly dead-lettered). Must exceed the
    /// worst-case transcode ladder (probe 30s + up to three 120s ffmpeg runs ≈
    /// 390s); 900s keeps the headroom without touching per-run bounds.
    #[serde(default = "default_jobs_job_timeout_secs")]
    pub jobs_job_timeout_secs: u64,
    /// `-threads` passed to every ffmpeg invocation.
    #[serde(default = "default_ffmpeg_threads")]
    pub ffmpeg_threads: u32,
    /// When the deployment terminates TLS (Caddy/certbot), session cookies get
    /// the `Secure` flag. Default `false` for plain-HTTP LAN dev; set
    /// `TOOTTOK_BEHIND_TLS=1` (or `behind_tls = true` in toottok.toml) behind a
    /// real TLS front.
    #[serde(default = "default_behind_tls")]
    pub behind_tls: bool,
    /// IPs of trusted reverse proxies (e.g. the Caddy container in the
    /// compose deploy). When the direct peer is one of these, the client IP is
    /// taken from the leftmost `X-Forwarded-For` hop so rate limits key off the
    /// real client. Default empty: the direct peer is the client (direct
    /// exposure, LAN/dev) and `X-Forwarded-For` is never trusted. Set
    /// `trusted_proxies = ["<caddy ip>"]` in toottok.toml (or
    /// `TOOTTOK_TRUSTED_PROXIES=<caddy ip>[,<more ip>]`) when behind
    /// compose/Caddy.
    #[serde(default = "default_trusted_proxies")]
    pub trusted_proxies: Vec<IpAddr>,
    /// Public hostname used to build canonical federation URLs
    /// (`https://{domain}/users/{u}`, WebFinger `acct:{u}@{domain}`,
    /// NodeInfo links). Default `toottok.test`.
    #[serde(default = "default_domain")]
    pub domain: String,
    /// Optional public port appended to federation URLs (`https://{domain}:{port}`)
    /// for dev installs without a reverse proxy on :443. When set, the server
    /// runs the federation crate in debug mode (http + localhost allowed).
    #[serde(default)]
    pub public_port: Option<u16>,
    /// Dev/test escape hatch for the egress guard: permits loopback peers and
    /// the `http` scheme so the two-instance localhost federation rig works.
    /// NEVER set this in production.
    #[serde(default = "default_allow_insecure_loopback_peers")]
    pub allow_insecure_loopback_peers: bool,
}

fn default_domain() -> String {
    "toottok.test".into()
}
fn default_allow_insecure_loopback_peers() -> bool {
    false
}

fn default_bind() -> String {
    "127.0.0.1:8080".into()
}
fn default_db() -> String {
    "postgres://toottok:toottok@127.0.0.1:5433/toottok_dev".into()
}
fn default_media_dir() -> String {
    "./media".into()
}
fn default_web_dir() -> String {
    "./web/build".into()
}
fn default_worker_concurrency() -> usize {
    2
}
fn default_jobs_job_timeout_secs() -> u64 {
    900
}
fn default_ffmpeg_threads() -> u32 {
    2
}
fn default_behind_tls() -> bool {
    false
}
fn default_trusted_proxies() -> Vec<IpAddr> {
    Vec::new()
}

impl Default for Config {
    fn default() -> Self {
        toml::from_str("").expect("empty toml parses to the serde defaults")
    }
}

impl Config {
    /// Public base URL for canonical federation objects, e.g.
    /// `https://toottok.test` or (dev) `http://toottok.test:8080`.
    pub fn public_base_url(&self) -> String {
        let scheme = if self.behind_tls { "https" } else { "http" };
        match self.public_port {
            Some(port) => format!("{scheme}://{}:{port}", self.domain),
            None => format!("{scheme}://{}", self.domain),
        }
    }

    /// The federation domain handed to the crate: `host` normally, `host:port`
    /// when a public port is configured (matching the crate's `is_local_url`
    /// comparison of `host:port`).
    pub fn federation_domain(&self) -> String {
        match self.public_port {
            Some(port) => format!("{}:{port}", self.domain),
            None => self.domain.clone(),
        }
    }

    /// True when the federation crate should run in debug mode: http + localhost
    /// URLs allowed. Only for dev installs (public port set or loopback peers
    /// permitted).
    pub fn federation_debug(&self) -> bool {
        self.public_port.is_some() || self.allow_insecure_loopback_peers
    }

    pub fn load() -> anyhow::Result<Self> {
        let mut cfg: Config = match std::fs::read_to_string("toottok.toml") {
            Ok(s) => toml::from_str(&s)?,
            Err(_) => toml::from_str("")?,
        };
        if let Ok(db) = std::env::var("DATABASE_URL") {
            cfg.database_url = db;
        }
        if let Ok(bind) = std::env::var("TOOTTOK_BIND") {
            cfg.bind_addr = bind;
        }
        if let Ok(dir) = std::env::var("TOOTTOK_MEDIA_DIR") {
            cfg.media_dir = dir;
        }
        if let Ok(conc) = std::env::var("TOOTTOK_WORKER_CONCURRENCY") {
            if let Ok(parsed) = conc.parse::<usize>() {
                cfg.worker_concurrency = parsed;
            }
        }
        if let Ok(secs) = std::env::var("TOOTTOK_JOBS_JOB_TIMEOUT_SECS") {
            if let Ok(parsed) = secs.parse::<u64>() {
                cfg.jobs_job_timeout_secs = parsed;
            }
        }
        if let Ok(threads) = std::env::var("TOOTTOK_FFMPEG_THREADS") {
            if let Ok(parsed) = threads.parse::<u32>() {
                cfg.ffmpeg_threads = parsed;
            }
        }
        if let Ok(tls) = std::env::var("TOOTTOK_BEHIND_TLS") {
            if let Ok(parsed) = tls.parse::<bool>() {
                cfg.behind_tls = parsed;
            }
        }
        if let Ok(proxies) = std::env::var("TOOTTOK_TRUSTED_PROXIES") {
            let parsed: Vec<IpAddr> = proxies
                .split(',')
                .filter_map(|p| p.trim().parse::<IpAddr>().ok())
                .collect();
            if !parsed.is_empty() {
                cfg.trusted_proxies = parsed;
            }
        }
        if let Ok(domain) = std::env::var("TOOTTOK_DOMAIN") {
            if !domain.trim().is_empty() {
                cfg.domain = domain.trim().to_string();
            }
        }
        if let Ok(port) = std::env::var("TOOTTOK_PUBLIC_PORT") {
            if let Ok(parsed) = port.trim().parse::<u16>() {
                cfg.public_port = Some(parsed);
            }
        }
        if let Ok(flag) = std::env::var("TOOTTOK_ALLOW_LOOPBACK_PEERS") {
            if let Ok(parsed) = flag.parse::<bool>() {
                cfg.allow_insecure_loopback_peers = parsed;
            }
        }
        Ok(cfg)
    }
}
