//! Outbound email abstraction (decision D5). The SMTP transport is a
//! feature-gated later phase; for now every deployment gets [`LogMailer`],
//! which writes the message to the structured log.
//!
//! ⚠ SECURITY: `LogMailer` emits the full message body, including the
//! plaintext verify/reset token, to the log. Tokens are printed at `debug!`
//! level, but production log retention still stores them — treat toottok logs
//! as containing secrets, rotate tokens when logs are shared, and never run
//! with a log level below `debug` in production.

/// A mail sink. Implementations must be `Send + Sync` to live behind
/// `Arc<dyn Mailer>` in `AppState`.
pub trait Mailer: Send + Sync {
    /// Deliver a plaintext message to `to`.
    fn send(&self, to: &str, subject: &str, body: &str);
}

/// Development/local default: trace the message at `debug!` level.
#[derive(Debug, Default, Clone)]
pub struct LogMailer;

impl Mailer for LogMailer {
    fn send(&self, to: &str, subject: &str, body: &str) {
        tracing::debug!(to = %to, subject = %subject, message = %body, "mail.out");
    }
}
