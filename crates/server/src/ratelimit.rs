//! In-memory per-IP token-bucket rate limiting (ARCHITECTURE.md §6). A fixed
//! 60s window per `IpAddr` guarded by a `Mutex` is sufficient for the
//! single-process v1; a distributed limiter (Redis/DB) is a later concern.
//! Over-limit requests answer `429 problem+json` with a `Retry-After` header.
//!
//! Route classes: `auth` (10/min), `upload` (6/min), `accounts` (60/min),
//! `admin` (30/min), everything else `120/min`.
//!
//! Client IP resolution honors `Config::trusted_proxies`: when the direct
//! peer is a listed reverse proxy the leftmost `X-Forwarded-For` hop is used,
//! otherwise the direct peer is the client (default: no proxy, no XFF trust).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request};
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::problem::problem;

#[derive(Debug, Clone, Copy)]
struct Window {
    start: Instant,
    count: u32,
}

/// One bucket per client IP, resetting every minute. `Clone` shares the
/// underlying map via `Arc`, so every request in a process counts together.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<IpAddr, Window>>>,
    limit: u32,
    window: Duration,
    trusted_proxies: Vec<IpAddr>,
}

impl RateLimiter {
    pub fn new(limit_per_min: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            limit: limit_per_min,
            window: Duration::from_secs(60),
            trusted_proxies: Vec::new(),
        }
    }

    /// Signup/login attempts: 10/minute per IP.
    pub fn auth() -> Self {
        Self::new(10)
    }

    /// Uploads: 6/minute per IP.
    pub fn upload() -> Self {
        Self::new(6)
    }

    /// `/api/v1/accounts/*`: 60/minute per IP.
    pub fn accounts() -> Self {
        Self::new(60)
    }

    /// `/api/v1/admin/*`: 30/minute per IP.
    pub fn admin() -> Self {
        Self::new(30)
    }

    /// Everything else.
    pub fn general() -> Self {
        Self::new(120)
    }

    /// Trusted reverse-proxy peers: when the direct peer is listed here the
    /// client IP is read from the leftmost `X-Forwarded-For` hop. Empty by
    /// default (direct peer = client).
    pub fn with_trusted_proxies(mut self, proxies: Vec<IpAddr>) -> Self {
        self.trusted_proxies = proxies;
        self
    }

    /// Record a hit for `ip`. `Ok(())` when under the limit; `Err(retry_after_secs)`
    /// when the window is exhausted.
    pub fn check(&self, ip: IpAddr) -> Result<(), u64> {
        let now = Instant::now();
        let mut buckets = self.inner.lock().expect("ratelimit mutex poisoned");
        let bucket = buckets.entry(ip).or_insert(Window {
            start: now,
            count: 0,
        });
        if now.duration_since(bucket.start) >= self.window {
            *bucket = Window {
                start: now,
                count: 0,
            };
        }
        if bucket.count >= self.limit {
            let retry = self
                .window
                .saturating_sub(now.duration_since(bucket.start))
                .as_secs();
            return Err(retry.max(1));
        }
        bucket.count += 1;
        Ok(())
    }
}

/// Run one request through `limiter`, answering 429 problem+json (with
/// `Retry-After`) when the bucket is exhausted. Used by the route middleware.
pub(crate) async fn apply(limiter: RateLimiter, req: Request, next: Next) -> Response {
    match limiter.check(client_ip(&req, &limiter.trusted_proxies)) {
        Ok(()) => next.run(req).await,
        Err(retry_after) => rate_limited(retry_after),
    }
}

/// Leftmost `X-Forwarded-For` hop, when present and parseable as an `IpAddr`.
fn forwarded_first_hop(req: &Request) -> Option<IpAddr> {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next().map(str::trim))
        .and_then(|first| first.parse::<IpAddr>().ok())
}

/// Client IP resolution:
/// - Direct peer (`ConnectInfo`, the real socket peer in production).
///   - If the peer is a trusted reverse proxy AND `X-Forwarded-For` is present,
///     the leftmost hop is the client (spoofing only possible by the proxy).
///   - Otherwise the peer is the client (direct-exposure default).
/// - No direct peer (oneshot tests): legacy `X-Forwarded-For` fallback, then
///   loopback.
fn client_ip(req: &Request, trusted_proxies: &[IpAddr]) -> IpAddr {
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        let peer = addr.ip();
        if trusted_proxies.contains(&peer) {
            if let Some(xff) = forwarded_first_hop(req) {
                return xff;
            }
        }
        return peer;
    }
    if let Some(xff) = forwarded_first_hop(req) {
        return xff;
    }
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}

fn rate_limited(retry_after: u64) -> Response {
    let mut resp = problem(
        StatusCode::TOO_MANY_REQUESTS,
        "rate limited",
        format!("too many requests; retry after {retry_after}s"),
    );
    resp.headers_mut().insert(
        header::RETRY_AFTER,
        retry_after
            .to_string()
            .parse()
            .expect("Retry-After is a valid header value"),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use std::str::FromStr;

    fn with_connect_info(mut req: HttpRequest<Body>, ip: IpAddr) -> HttpRequest<Body> {
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip, 40000)));
        req
    }

    fn request(peer: IpAddr, xff: Option<&str>) -> HttpRequest<Body> {
        let mut builder = HttpRequest::builder();
        if let Some(xff) = xff {
            builder = builder.header("x-forwarded-for", xff);
        }
        with_connect_info(builder.body(Body::empty()).expect("request"), peer)
    }

    #[test]
    fn direct_peer_is_client_when_no_proxies_trusted() {
        let client: IpAddr = "203.0.113.7".parse().unwrap();
        let req = request(client, Some("198.51.100.9, 10.0.0.1"));
        assert_eq!(
            client_ip(&req, &[]),
            client,
            "XFF must be ignored without trusted proxies"
        );
    }

    #[test]
    fn trusted_proxy_honors_xff_leftmost() {
        let proxy: IpAddr = "10.0.0.5".parse().unwrap();
        let client: IpAddr = "198.51.100.9".parse().unwrap();
        let trusted = vec![proxy];
        let req = request(proxy, Some("198.51.100.9, 10.0.0.1"));
        assert_eq!(
            client_ip(&req, &trusted),
            client,
            "leftmost XFF hop is the client"
        );
    }

    #[test]
    fn trusted_proxy_without_xff_falls_back_to_peer() {
        let proxy: IpAddr = "10.0.0.5".parse().unwrap();
        let trusted = vec![proxy];
        let req = request(proxy, None);
        assert_eq!(client_ip(&req, &trusted), proxy);
    }

    #[test]
    fn untrusted_peer_is_client_even_with_xff() {
        let peer: IpAddr = "203.0.113.7".parse().unwrap();
        let trusted = vec![IpAddr::from_str("10.0.0.5").unwrap()];
        let req = request(peer, Some("198.51.100.9"));
        assert_eq!(
            client_ip(&req, &trusted),
            peer,
            "untrusted peer's XFF is spoofable; ignore it"
        );
    }

    #[test]
    fn oneshot_without_connect_info_keeps_xff_fallback() {
        let mut req = HttpRequest::builder()
            .header("x-forwarded-for", "203.0.113.7")
            .body(Body::empty())
            .unwrap();
        req.extensions_mut().remove::<ConnectInfo<SocketAddr>>();
        assert_eq!(
            client_ip(&req, &[]),
            IpAddr::from_str("203.0.113.7").unwrap(),
            "tests/oneshot fallback preserved"
        );
    }
}
