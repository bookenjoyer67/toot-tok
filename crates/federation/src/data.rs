//! Application data handed to every federation handler and fetch. This is the
//! crate's `app_data` (`Data<FederationData>`), shared with the axum router via
//! the `FederationMiddleware`.

/// Data required by the ActivityPub federation handlers and by outbound
/// delivery. Cloned into every inbox/GET request.
#[derive(Clone)]
pub struct FederationData {
    /// Database pool (always present when federation is on).
    pub pool: sqlx::PgPool,
    /// The federation domain as seen by the crate — `host` for production,
    /// `host:port` for the localhost dev/test rig (`config.domain`).
    pub domain: String,
    /// Public base URL for local objects, e.g. `https://toottok.test` or
    /// `http://localhost:8095`.
    pub base_url: String,
    /// When true, the crate runs in `debug` mode (allows `http` + localhost
    /// URLs); the egress guard also permits loopback. Dev/test only.
    pub allow_loopback: bool,
}
