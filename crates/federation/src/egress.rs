//! Egress guard — DNS-rebinding-proof outbound HTTP.
//!
//! ARCHITECTURE.md §4: resolve the host ONCE, validate the resolved address
//! against the blocked ranges, then connect to the PINNED address via a reqwest
//! client built with [`reqwest::ClientBuilder::resolve`] so the connection can
//! never re-resolve to a different (attacker-chosen) address at connect time —
//! that TOCTOU is the documented bypass class of the upstream CVEs.
//!
//! Blocked: loopback, RFC1918 private, link-local, CGNAT (100.64.0.0/10),
//! multicast, documentation/TEST-NET, reserved and unspecified addresses, for
//! both IPv4 and IPv6. A single dev/test escape hatch
//! (`allow_insecure_loopback_peers`) permits loopback (and plain http) so the
//! two-instance cross-federation test rig can run on localhost.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::{redirect::Policy, Client};
use thiserror::Error;
use url::Url;

/// Errors surfaced by the egress guard.
#[derive(Debug, Error)]
pub enum EgressError {
    /// The URL has no host to resolve.
    #[error("url has no host: {0}")]
    NoHost(String),
    /// The hostname does not resolve to any address.
    #[error("host {0} did not resolve")]
    ResolutionFailed(String),
    /// The resolved address (or one of them) is in a blocked range.
    #[error("blocked address {0} for host {1}")]
    BlockedAddress(IpAddr, String),
    /// The URL scheme is not permitted by the current guard mode.
    #[error("scheme {0} not allowed")]
    SchemeNotAllowed(String),
    /// Underlying reqwest builder error (effectively infallible).
    #[error("client build failed: {0}")]
    ClientBuild(String),
    /// An address that is not IP literal could not be resolved at all.
    #[error("lookup failed: {0}")]
    Lookup(#[from] std::io::Error),
}

/// A reqwest client with a pinned, validated address for one `(host, port)`.
///
/// `client_for` resolves and validates the host of `url` (cached), then returns
/// a client whose DNS for that host is overridden to the pinned address. The
/// URL's hostname is still used for TLS SNI and the `Host` header, so
/// certificate verification keeps working against the original name.
pub struct EgressGuard {
    allow_loopback: bool,
    allow_http: bool,
    /// `(host, port)` -> validated pinned socket addr.
    pinned: Mutex<HashMap<(String, u16), SocketAddr>>,
    /// `(host, port)` -> reqwest client configured with `.resolve`.
    clients: Mutex<HashMap<(String, u16), Client>>,
}

impl Clone for EgressGuard {
    fn clone(&self) -> Self {
        // `std::sync::Mutex` is not Clone; clone the cached maps' contents.
        EgressGuard {
            allow_loopback: self.allow_loopback,
            allow_http: self.allow_http,
            pinned: Mutex::new(self.pinned.lock().map(|m| m.clone()).unwrap_or_default()),
            clients: Mutex::new(self.clients.lock().map(|m| m.clone()).unwrap_or_default()),
        }
    }
}

impl EgressGuard {
    /// `allow_insecure_loopback_peers` is the dev/test escape hatch: when
    /// `true`, loopback addresses and the `http` scheme are permitted (so the
    /// localhost cross-instance rig works). Never enable in production.
    pub fn new(allow_loopback: bool) -> Self {
        Self {
            allow_loopback,
            allow_http: allow_loopback,
            pinned: Mutex::new(HashMap::new()),
            clients: Mutex::new(HashMap::new()),
        }
    }

    /// Resolve `url`'s host once and return the pinned, validated address.
    ///
    /// Every caller (delivery worker, remote-actor fetch) MUST then route its
    /// request through a client produced by [`EgressGuard::client_for`] for that
    /// same URL — never a generic client that would re-resolve.
    pub async fn validate_and_pin(&self, url: &Url) -> Result<SocketAddr, EgressError> {
        let scheme = url.scheme();
        if scheme != "https" && !(scheme == "http" && self.allow_http) {
            return Err(EgressError::SchemeNotAllowed(scheme.to_string()));
        }
        let Some(host) = url.host_str() else {
            return Err(EgressError::NoHost(url.to_string()));
        };
        let port = url.port().unwrap_or(match scheme {
            "https" => 443,
            _ => 80,
        });

        if let Some(pinned) = self
            .pinned
            .lock()
            .ok()
            .and_then(|m| m.get(&(host.to_string(), port)).copied())
        {
            return Ok(pinned);
        }

        let mut resolved: Vec<IpAddr> = match url.host() {
            Some(url::Host::Ipv4(ip)) => vec![IpAddr::V4(ip)],
            Some(url::Host::Ipv6(ip)) => vec![IpAddr::V6(ip)],
            _ => tokio::net::lookup_host((host, port))
                .await?
                .map(|sa| sa.ip())
                .collect(),
        };
        if resolved.is_empty() {
            return Err(EgressError::ResolutionFailed(host.to_string()));
        }
        // Prefer a globally routable address, then IPv4 over IPv6 (keeps the
        // choice deterministic when a host resolves to both loopbacks or both
        // a public A and AAAA — IPv4 is the safest default for the pinned
        // connection). Falls back to the first address validation accepts.
        resolved.sort_by_key(|ip| (!is_global(*ip), !ip.is_ipv4()));
        for ip in &resolved {
            if ip_is_blocked(*ip, self.allow_loopback) {
                continue;
            }
            let addr = SocketAddr::new(*ip, port);
            let _ = self
                .pinned
                .lock()
                .map(|mut m| m.insert((host.to_string(), port), addr));
            return Ok(addr);
        }
        Err(EgressError::BlockedAddress(resolved[0], host.to_string()))
    }

    /// Get (building and caching on first use) a reqwest client whose DNS entry
    /// for `url`'s host is pinned to the address validated by
    /// [`EgressGuard::validate_and_pin`].
    pub async fn client_for(&self, url: &Url) -> Result<Client, EgressError> {
        let addr = self.validate_and_pin(url).await?;
        let Some(host) = url.host_str() else {
            return Err(EgressError::NoHost(url.to_string()));
        };
        let port = url
            .port()
            .unwrap_or(if url.scheme() == "https" { 443 } else { 80 });
        let key = (host.to_string(), port);

        if let Some(client) = self.clients.lock().ok().and_then(|m| m.get(&key).cloned()) {
            return Ok(client);
        }

        let client = Client::builder()
            .resolve(host, addr)
            .redirect(Policy::none())
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("toottok/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| EgressError::ClientBuild(e.to_string()))?;
        let _ = self
            .clients
            .lock()
            .map(|mut m| m.insert(key, client.clone()));
        Ok(client)
    }
}

/// True when `ip` is in a blocked range (or loopback is allowed and the address
/// is loopback). Pure function so the guard's policy is unit-testable without
/// any DNS involvement.
pub fn ip_is_blocked(ip: IpAddr, allow_loopback: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_is_blocked(v4, allow_loopback),
        IpAddr::V6(v6) => v6_is_blocked(v6, allow_loopback),
    }
}

/// Validate a remote media URL BEFORE it is stored as `remote_media_url`
/// (F13): the scheme must be https (http only under the dev/test flag), the
/// URL must carry a host, and every resolved address must pass
/// [`ip_is_blocked`] — so `javascript:` URIs, bare-host junk, and
/// link-local/private targets (169.254.x, metadata endpoints) are rejected at
/// ingest instead of being handed to clients or fetched later. Returns a
/// human-readable rejection reason on failure.
pub async fn validate_media_ingest_url(raw: &str, allow_loopback: bool) -> Result<(), String> {
    let url = Url::parse(raw).map_err(|e| format!("unparseable media url: {e}"))?;
    match url.scheme() {
        "https" => {}
        "http" if allow_loopback => {}
        other => return Err(format!("media url scheme {other:?} not allowed")),
    }
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| "media url has no host".to_string())?;

    // IP literals skip DNS entirely.
    if let Ok(ip) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
        return if ip_is_blocked(ip, allow_loopback) {
            Err(format!("media host {ip} is in a blocked range"))
        } else {
            Ok(())
        };
    }

    let port = url.port().unwrap_or(match url.scheme() {
        "https" => 443,
        _ => 80,
    });
    let addrs: Vec<IpAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("media host {host} did not resolve: {e}"))?
        .map(|sa| sa.ip())
        .collect();
    if addrs.is_empty() {
        return Err(format!("media host {host} did not resolve"));
    }
    for ip in addrs {
        if ip_is_blocked(ip, allow_loopback) {
            return Err(format!(
                "media host {host} resolves to blocked address {ip}"
            ));
        }
    }
    Ok(())
}

fn v4_is_blocked(v4: Ipv4Addr, allow_loopback: bool) -> bool {
    if allow_loopback && v4.is_loopback() {
        return false;
    }
    let octets = v4.octets();
    // 100.64.0.0/10 CGNAT
    if octets[0] == 100 && (octets[1] & 0b1100_0000) == 0b0100_0000 {
        return true;
    }
    v4.is_unspecified()
        || v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_documentation()
        || v4.is_multicast()
        // 192.0.0.0/24 protocol assignments (excl. 192.0.0.9/10 globally anycast)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        // 198.18.0.0/15 benchmarking
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        // 255.255.255.255 covered by is_broadcast; 240.0.0.0/4 reserved
        || (octets[0] >= 240)
}

fn v6_is_blocked(v6: Ipv6Addr, allow_loopback: bool) -> bool {
    if allow_loopback && v6.is_loopback() {
        return false;
    }
    if let Some(mapped) = v6.to_ipv4_mapped() {
        return v4_is_blocked(mapped, allow_loopback);
    }
    v6.is_unspecified()
        || v6.is_loopback()
        || v6.is_unique_local()
        || v6.is_unicast_link_local()
        || v6.is_multicast()
        // documentation ranges 2001:db8::/32 and 3fff::/20
        || matches!(v6.segments(), [0x2001, 0xdb8, ..] | [0x3fff, 0..=0x0fff, ..])
}

/// Best-effort "globally routable" test (docs warn `is_global` is not
/// stabilized yet): global unicast for v4 is everything not otherwise special;
/// for v6 it is 2000::/3.
fn is_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            !(v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || (octets[0] == 100 && (octets[1] & 0b1100_0000) == 0b0100_0000)
                || octets[0] >= 224)
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            (segments[0] & 0xe000) == 0x2000 && !v6.is_unicast_link_local() && !v6.is_unique_local()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    fn v6(s: &[u16; 8]) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ))
    }

    const LOOPBACK_V4: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
    const LOOPBACK_V6: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);

    #[test]
    fn rejects_unspecified_ipv4() {
        assert!(ip_is_blocked(v4(0, 0, 0, 0), false));
    }

    #[test]
    fn rejects_loopback_v4_and_v6_by_default() {
        assert!(ip_is_blocked(LOOPBACK_V4, false));
        assert!(ip_is_blocked(LOOPBACK_V6, false));
    }

    #[test]
    fn loopback_allowed_only_under_flag() {
        assert!(!ip_is_blocked(LOOPBACK_V4, true));
        assert!(!ip_is_blocked(LOOPBACK_V6, true));
        assert!(ip_is_blocked(v4(127, 5, 5, 5), false));
        assert!(!ip_is_blocked(v4(127, 5, 5, 5), true));
    }

    #[test]
    fn public_ipv4_ok() {
        assert!(!ip_is_blocked(v4(8, 8, 8, 8), false));
        assert!(!ip_is_blocked(v4(172, 66, 147, 243), false));
    }

    #[test]
    fn rejects_private_ranges() {
        for ip in [
            v4(10, 0, 0, 1),
            v4(172, 16, 0, 1),
            v4(172, 31, 255, 255),
            v4(192, 168, 0, 1),
        ] {
            assert!(ip_is_blocked(ip, false), "{ip} should be blocked");
        }
    }

    #[test]
    fn rejects_cgnat() {
        for ip in [v4(100, 64, 0, 1), v4(100, 127, 255, 254)] {
            assert!(ip_is_blocked(ip, false), "{ip} should be blocked");
        }
        // 100.63.x / 100.128.x are NOT cgnat
        assert!(!ip_is_blocked(v4(100, 63, 0, 1), false));
        assert!(!ip_is_blocked(v4(100, 128, 0, 1), false));
    }

    #[test]
    fn rejects_link_local_multicast_reserved_documentation() {
        for ip in [
            v4(169, 254, 1, 1),
            v4(224, 0, 0, 1),
            v4(240, 0, 0, 1),
            v4(192, 0, 2, 1),
            v4(198, 18, 0, 1),
            v4(255, 255, 255, 255),
        ] {
            assert!(ip_is_blocked(ip, false), "{ip} should be blocked");
        }
    }

    #[test]
    fn rejects_ipv6_special_ranges() {
        let cases = [
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            v6(&[0xfe80, 0, 0, 0, 0, 0, 0, 1]),     // link-local
            v6(&[0xfc00, 0, 0, 0, 0, 0, 0, 1]),     // unique-local
            v6(&[0xff00, 0, 0, 0, 0, 0, 0, 1]),     // multicast
            v6(&[0x2001, 0xdb8, 0, 0, 0, 0, 0, 1]), // documentation
        ];
        for ip in cases {
            assert!(ip_is_blocked(ip, false), "{ip} should be blocked");
        }
        assert!(!ip_is_blocked(
            v6(&[0x2606, 0x4700, 0, 0, 0, 0, 0, 1]),
            false
        ));
    }

    #[test]
    fn rejects_ipv4_mapped_loopback_in_v6() {
        let mapped = IpAddr::V6("::ffff:127.0.0.1".parse().unwrap());
        assert!(ip_is_blocked(mapped, false));
        assert!(!ip_is_blocked(mapped, true));
    }

    #[tokio::test]
    async fn pin_literal_ip_urls() {
        let guard = EgressGuard::new(false);
        let ok = guard
            .validate_and_pin(&Url::parse("https://8.8.8.8/x").unwrap())
            .await
            .expect("public literal ip pins");
        assert_eq!(ok.ip(), v4(8, 8, 8, 8));

        let err = guard
            .validate_and_pin(&Url::parse("https://0.0.0.0/x").unwrap())
            .await;
        assert!(matches!(err, Err(EgressError::BlockedAddress(_, _))));

        let err = guard
            .validate_and_pin(&Url::parse("https://[::1]/x").unwrap())
            .await;
        assert!(matches!(err, Err(EgressError::BlockedAddress(_, _))));
    }

    #[tokio::test]
    async fn loopback_pins_only_under_flag() {
        let strict = EgressGuard::new(false);
        let err = strict
            .validate_and_pin(&Url::parse("http://127.0.0.1:8080/x").unwrap())
            .await;
        // scheme blocked first (http not allowed when loopback not allowed)
        assert!(err.is_err());

        let dev = EgressGuard::new(true);
        let addr = dev
            .validate_and_pin(&Url::parse("http://localhost:8123/x").unwrap())
            .await
            .expect("localhost pins under dev flag");
        assert!(addr.ip().is_loopback());
        assert_eq!(addr.port(), 8123);

        let client = dev
            .client_for(&Url::parse("http://localhost:8123/x").unwrap())
            .await
            .expect("client builds");
        assert_eq!(
            client
                .get("http://localhost:8123/x")
                .build()
                .unwrap()
                .url()
                .host_str(),
            Some("localhost")
        );
    }

    #[tokio::test]
    async fn http_scheme_rejected_without_flag() {
        let guard = EgressGuard::new(false);
        let err = guard
            .validate_and_pin(&Url::parse("http://example.com/x").unwrap())
            .await;
        assert!(matches!(err, Err(EgressError::SchemeNotAllowed(_))));
    }

    #[tokio::test]
    async fn media_ingest_url_validation_rejects_dangerous_targets() {
        // F13: javascript:/data: URIs and non-https schemes are dead on arrival.
        for bad in ["javascript:alert(1)", "data:video/mp4;base64,AAAA"] {
            let err = validate_media_ingest_url(bad, false).await;
            assert!(err.is_err(), "{bad} must be rejected");
        }

        // http only under the dev flag.
        assert!(
            validate_media_ingest_url("http://cdn.example.com/v.mp4", false)
                .await
                .is_err()
        );
        assert!(
            validate_media_ingest_url("http://localhost:8123/v.mp4", true)
                .await
                .is_ok(),
            "dev flag permits plain-http loopback media"
        );

        // Link-local / private / loopback literals are blocked.
        for host in [
            "169.254.169.254",
            "10.0.0.7",
            "192.168.1.10",
            "127.0.0.1",
            "[::1]",
        ] {
            let url = format!("https://{host}/v.mp4");
            assert!(
                validate_media_ingest_url(&url, false).await.is_err(),
                "{url} must be rejected"
            );
        }

        // Public literal passes; missing host fails.
        assert!(validate_media_ingest_url("https://8.8.8.8/v.mp4", false)
            .await
            .is_ok());
        assert!(validate_media_ingest_url("https:///v.mp4", false)
            .await
            .is_err());
    }
}
