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
    /// Common Log Format timestamp, e.g. `02/Sep/2026:07:30:00 +0000`.
    ///
    /// Carried on the event rather than added by the subscriber, because the lancache format
    /// places it positionally inside the line and dashboards parse it there.
    pub timestamp: String,
}

/// Format a Unix timestamp the way Common Log Format expects.
///
/// Hand-rolled rather than pulling in a date library: this is the only place cachic formats a
/// time, the format is fixed, and UTC means no zone database is involved.
pub fn clf_timestamp(unix_seconds: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = unix_seconds / 86_400;
    let secs_of_day = unix_seconds % 86_400;
    let (hour, minute, second) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );

    // Civil-from-days, the standard algorithm; shifts the epoch to 0000-03-01 so leap days land
    // at the end of the cycle.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!(
        "{:02}/{}/{:04}:{:02}:{:02}:{:02} +0000",
        d,
        MONTHS[(m - 1) as usize],
        year,
        hour,
        minute,
        second
    )
}

impl AccessEvent {
    /// monolithic's `cachelog` format, field for field.
    ///
    /// The shape is not ours to choose: existing dashboards parse it positionally, so a
    /// well-meaning improvement here breaks them. It is covered by a fixture test for that
    /// reason.
    pub fn to_lancache(&self) -> String {
        let mut out = String::with_capacity(160);
        let _ = write!(
            out,
            "[{}] {} / - - - [{}] \"{} {} HTTP/1.1\" {} {} \"-\" \"{}\" \"{}\"",
            self.service,
            self.client_ip,
            self.timestamp,
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
                timestamp = %self.timestamp,
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
            .with(
                // Bare lines: no timestamp, no level, no target. LANCache Manager and friends
                // parse this positionally, and a leading "INFO " shifts every field. The
                // timestamp lives inside the line itself, where the format puts it.
                fmt::layer()
                    .without_time()
                    .with_target(false)
                    .with_level(false),
            )
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
            timestamp: clf_timestamp(1_756_800_000),
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
    fn formats_common_log_timestamps() {
        // Spot values across a leap year and a century boundary, since the civil-from-days
        // algorithm is exactly where a hand-rolled date formatter goes wrong.
        assert_eq!(clf_timestamp(0), "01/Jan/1970:00:00:00 +0000");
        assert_eq!(clf_timestamp(86_399), "01/Jan/1970:23:59:59 +0000");
        assert_eq!(clf_timestamp(86_400), "02/Jan/1970:00:00:00 +0000");
        // 2000-02-29, a leap year despite being divisible by 100.
        assert_eq!(clf_timestamp(951_782_400), "29/Feb/2000:00:00:00 +0000");
        // 2024-02-29, an ordinary leap year.
        assert_eq!(clf_timestamp(1_709_164_800), "29/Feb/2024:00:00:00 +0000");
        assert_eq!(clf_timestamp(1_735_689_600), "01/Jan/2025:00:00:00 +0000");
    }

    #[test]
    fn the_timestamp_appears_where_dashboards_expect_it() {
        let line = event().to_lancache();
        assert!(
            line.contains("[02/Sep/2025:"),
            "timestamp missing or misplaced: {line}"
        );
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
