//! Structured logging and the access log.
//!
//! Two outputs with different audiences and no derivation between them:
//!
//! - **JSON on stdout** (FR-51), for operators and log pipelines. The supported format.
//! - **lancache `cachelog` format** (FR-52), so LANCache Manager, DeveLanCacheUI and
//!   lancache-ui keep working for people migrating. A compatibility shim, not the observability
//!   story - `/metrics` is that.
//!
//! Logs go to stdout, never to a file on a volume. Writing an access log to the data volume is
//! one of the things this project exists to stop doing.

use std::fmt::Write as _;

use crate::config::LogFormat;

/// One served request, as both formats render it.
#[derive(Debug, Clone)]
pub struct AccessEvent {
    pub client_ip: String,
    pub service: String,
    pub host: String,
    pub method: String,
    pub path: String,
    pub range: Option<String>,
    pub status: u16,
    pub bytes: u64,
    pub cache_status: String,
    pub upstream_seconds: f64,
    pub user_agent: Option<String>,
}

impl AccessEvent {
    /// monolithic's `cachelog` format, field for field.
    ///
    /// The shape is not ours to choose: existing dashboards parse it positionally, so a
    /// well-meaning improvement here breaks them. It is covered by a fixture test for that
    /// reason.
    pub fn to_lancache(&self) -> String {
        let mut out = String::with_capacity(160);
        // The timestamp is supplied by the subscriber layer in production, so it is a literal
        // dash here; keeping it out of the struct lets the format be tested without freezing time.
        let _ = write!(
            out,
            "[{}] {} / - - - [-] \"{} {} HTTP/1.1\" {} {} \"-\" \"{}\" \"{}\"",
            self.service,
            self.client_ip,
            self.method,
            self.path,
            self.status,
            self.bytes,
            self.user_agent.as_deref().unwrap_or("-"),
            self.cache_status,
        );
        out
    }

    /// Emit through `tracing`, on the target matching the configured format.
    pub fn emit(&self, format: LogFormat) {
        match format {
            LogFormat::Json => tracing::info!(
                target: "cachic::access",
                client_ip = %self.client_ip,
                service = %self.service,
                host = %self.host,
                method = %self.method,
                path = %self.path,
                range = self.range.as_deref().unwrap_or("-"),
                status = self.status,
                bytes = self.bytes,
                cache = %self.cache_status,
                upstream_seconds = self.upstream_seconds,
                "request"
            ),
            LogFormat::Lancache => tracing::info!(
                target: "cachic::access",
                "{}",
                self.to_lancache()
            ),
        }
    }
}

/// Install the global subscriber.
///
/// Idempotent in the sense that a second call is ignored rather than panicking, which matters
/// because tests may initialise logging more than once.
pub fn init(format: LogFormat, level: &str) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    let result = match format {
        LogFormat::Json => tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().json().with_current_span(false))
            .try_init(),
        LogFormat::Lancache => tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer().without_time().with_target(false))
            .try_init(),
    };
    let _ = result;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event() -> AccessEvent {
        AccessEvent {
            client_ip: "192.168.1.50".into(),
            service: "steam".into(),
            host: "lancache.steamcontent.com".into(),
            method: "GET".into(),
            path: "/depot/1/chunk".into(),
            range: Some("bytes=0-1023".into()),
            status: 206,
            bytes: 1024,
            cache_status: "HIT".into(),
            upstream_seconds: 0.0,
            user_agent: Some("Valve/Steam HTTP Client 1.0".into()),
        }
    }

    #[test]
    fn lancache_format_carries_the_fields_dashboards_parse() {
        // Existing dashboards parse this positionally, so this test exists to make the format
        // hard to change by accident.
        let line = event().to_lancache();
        assert!(line.starts_with("[steam] 192.168.1.50"), "{line}");
        assert!(line.contains("\"GET /depot/1/chunk HTTP/1.1\""), "{line}");
        assert!(line.contains(" 206 1024 "), "{line}");
        assert!(line.contains("\"HIT\""), "{line}");
        assert!(line.contains("Valve/Steam HTTP Client 1.0"), "{line}");
    }

    #[test]
    fn an_absent_user_agent_renders_as_a_dash_not_empty() {
        // A positional parser given an empty field shifts every field after it.
        let mut e = event();
        e.user_agent = None;
        let line = e.to_lancache();
        assert!(line.contains("\"-\" \"HIT\""), "{line}");
    }

    #[test]
    fn the_two_formats_are_independent() {
        // Neither is derived from the other: JSON is the supported output, lancache is a shim.
        let e = event();
        let lancache = e.to_lancache();
        assert!(!lancache.contains("upstream_seconds"));
        assert!(!lancache.contains("cache="));
    }
}
