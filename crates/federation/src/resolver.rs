//! DNS-level egress enforcement for the federation crate.
//!
//! The crate's internal fetches (signature keyId derefs, ObjectId
//! resolution) run on the client handed to `FederationConfigBuilder::client`.
//! By plugging a [`GuardedResolve`] into that client, EVERY outbound lookup —
//! ours and the crate's — passes through the same IP policy as
//! [`crate::egress::ip_is_blocked`], so no code path can re-resolve past the
//! guard (DNS-rebinding defense at resolution time, per the advisory class
//! documented in docs/research/fediverse-recon.md).

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tokio::task::spawn_blocking;

use crate::egress::ip_is_blocked;

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

/// A `reqwest::dns::Resolve` implementation enforcing the egress IP policy.
#[derive(Clone)]
pub struct GuardedResolve {
    allow_loopback: bool,
    /// Hosts explicitly exempted (dev/test rig: `localhost` instances).
    allow_hosts: Arc<HashSet<String>>,
}

impl GuardedResolve {
    pub fn new(allow_loopback: bool, allow_hosts: impl IntoIterator<Item = String>) -> Self {
        Self {
            allow_loopback,
            allow_hosts: Arc::new(allow_hosts.into_iter().collect()),
        }
    }

    fn check(&self, host: &str, ip: IpAddr) -> Result<(), String> {
        if self.allow_hosts.contains(host) {
            return Ok(());
        }
        if ip_is_blocked(ip, self.allow_loopback) {
            Err(format!("egress guard blocked {host} -> {ip}"))
        } else {
            Ok(())
        }
    }
}

impl Resolve for GuardedResolve {
    fn resolve(&self, name: Name) -> Resolving {
        let this = self.clone();
        let host = name.as_str().to_string();
        let fut: BoxFuture<'static, Result<Addrs, BoxErr>> = Box::pin(async move {
            // Resolve via the system resolver off-thread (gai), then filter.
            let lookup_host = host.clone();
            let joined: Result<Result<Vec<SocketAddr>, String>, tokio::task::JoinError> =
                spawn_blocking(move || {
                    std::net::ToSocketAddrs::to_socket_addrs(&(lookup_host.as_str(), 0))
                        .map(|it| it.collect::<Vec<_>>())
                        .map_err(|e| e.to_string())
                })
                .await;
            let addrs: Vec<SocketAddr> = match joined {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => return Err(BoxErr::from(format!("dns: {e}"))),
                Err(e) => return Err(BoxErr::from(format!("join error: {e}"))),
            };

            let mut allowed: Vec<SocketAddr> = Vec::new();
            for mut sa in addrs {
                sa.set_port(0);
                if this.check(&host, sa.ip()).is_ok() {
                    allowed.push(sa);
                } else {
                    tracing::debug!(%host, ip = %sa.ip(), "resolve candidate blocked");
                }
            }
            if allowed.is_empty() && !this.allow_hosts.contains(&host) {
                return Err(BoxErr::from(format!(
                    "egress guard: all candidates for {host} blocked"
                )));
            }
            let out: Addrs = Box::new(allowed.into_iter());
            Ok(out)
        });
        fut
    }
}

/// Convenience: build the shared guarded client used by BOTH our own fetches
/// and the crate's internals. Mirrors the crate's default_client hardening
/// (no redirects, timeouts) plus the DNS-level policy.
pub fn guarded_client(
    allow_loopback: bool,
    allow_hosts: impl IntoIterator<Item = String>,
) -> reqwest::Client {
    let resolver = Arc::new(GuardedResolve::new(allow_loopback, allow_hosts));
    reqwest::Client::builder()
        .dns_resolver(resolver)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("toottok/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| reqwest::Client::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(host: &str, ip: IpAddr, allow: bool) -> bool {
        GuardedResolve::new(allow, ["localhost.test".to_string()])
            .check(host, ip)
            .is_err()
    }

    #[test]
    fn blocks_dangerous_targets() {
        assert!(blocked("evil", "0.0.0.0".parse().unwrap(), false));
        assert!(blocked("x", "127.0.0.1".parse().unwrap(), false));
        assert!(blocked("x", "::1".parse().unwrap(), false));
        // v4-mapped loopback smuggled through v6
        assert!(blocked("x", "::ffff:127.0.0.1".parse().unwrap(), false));
        assert!(blocked("x", "::ffff:10.0.0.1".parse().unwrap(), false));
        // CGNAT 100.64/10
        assert!(blocked("x", "100.64.0.1".parse().unwrap(), false));
        assert!(blocked("x", "10.1.2.3".parse().unwrap(), false));
        assert!(blocked("x", "192.168.1.100".parse().unwrap(), false));
        assert!(blocked("x", "169.254.1.1".parse().unwrap(), false));
    }

    #[test]
    fn allows_public_and_flagged() {
        assert!(!blocked("fediverse", "8.8.8.8".parse().unwrap(), false));
        assert!(!blocked(
            "ok",
            "2606:4700:4700::1111".parse().unwrap(),
            false
        ));
        // dev flag opens loopback
        assert!(!blocked("x", "127.0.0.1".parse().unwrap(), true));
        // explicit host exemption beats policy even with flag off
        assert!(!blocked(
            "localhost.test",
            "127.0.0.1".parse().unwrap(),
            false
        ));
    }
}
