//! Prometheus metrics.
//!
//! Observability is a headline reason this project exists: every dashboard in the lancache
//! ecosystem is a log tailer because nginx gave them nothing else. cachic ships `/metrics` from
//! day one (FR-50).
//!
//! # Cardinality
//!
//! Labels are bounded by construction. Service names come from `cache-domains` and are a closed
//! set of a few dozen; URLs, hosts and client addresses are unbounded and must never become
//! labels. An unbounded label is not a style problem, it is an outage: it grows the scrape
//! payload and the server's memory without limit. There is a test asserting the label sets stay
//! closed.
//!
//! # foyer's counters
//!
//! foyer is given this same registry, so its internal counters appear alongside ours. Two matter
//! more than the rest:
//!
//! - `foyer_storage_queue_channel_overflow` - disk writes discarded because the flushers fell
//!   behind. Non-zero means the cache is silently declining to cache, which is this product's
//!   worst failure mode and is invisible without this counter. M0 found it the hard way; see
//!   `docs/benchmarks/m0/README.md`.
//! - `foyer_storage_block_engine_enqueue_skip` - entries skipped before reaching the queue.

use std::sync::Arc;

use mixtrics::registry::prometheus_0_14::PrometheusMetricsRegistry;
use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts, Registry, TextEncoder,
};

/// Every metric cachic records itself.
pub struct Metrics {
    registry: Registry,

    /// Requests by service and cache status. Both label sets are closed.
    pub requests: IntCounterVec,
    /// Bytes served to clients, by service and cache status.
    pub bytes_served: IntCounterVec,
    /// Bytes fetched from origins, by service.
    pub bytes_fetched: IntCounterVec,
    /// Upstream fetch latency by service.
    pub upstream_seconds: HistogramVec,
    /// Slice fetches currently in flight.
    pub inflight: IntGauge,
    /// Slices whose checksum failed on read, by service. Non-zero means corruption.
    pub checksum_failures: IntCounterVec,
    /// Object generations bumped because validators changed, by service.
    pub generation_bumps: IntCounterVec,
    /// Requests refused by the upstream address guard, by reason.
    pub guard_refusals: IntCounterVec,
    /// Open client connections.
    pub connections: IntGauge,
}

/// Cache status label values. Closed set, matching `X-Cache`.
pub const CACHE_STATUSES: &[&str] = &["HIT", "MISS", "PARTIAL", "BYPASS"];

impl Metrics {
    pub fn new() -> Result<(Self, Arc<PrometheusMetricsRegistry>), prometheus::Error> {
        let registry = Registry::new();

        let counter = |name: &str, help: &str, labels: &[&str]| {
            IntCounterVec::new(Opts::new(name, help), labels)
        };

        let requests = counter(
            "cachic_requests_total",
            "Client requests by service and cache status",
            &["service", "status"],
        )?;
        let bytes_served = counter(
            "cachic_bytes_served_total",
            "Bytes served to clients by service and cache status",
            &["service", "status"],
        )?;
        let bytes_fetched = counter(
            "cachic_bytes_fetched_total",
            "Bytes fetched from origins by service",
            &["service"],
        )?;
        let upstream_seconds = HistogramVec::new(
            HistogramOpts::new(
                "cachic_upstream_seconds",
                "Upstream slice fetch latency in seconds",
            )
            // Buckets spanning a LAN-local origin to a slow WAN fetch.
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
            ]),
            &["service"],
        )?;
        let inflight = IntGauge::new(
            "cachic_upstream_inflight",
            "Slice fetches currently in flight",
        )?;
        let checksum_failures = counter(
            "cachic_checksum_failures_total",
            "Slices whose checksum failed verification on read",
            &["service"],
        )?;
        let generation_bumps = counter(
            "cachic_generation_bumps_total",
            "Objects invalidated because their validators changed",
            &["service"],
        )?;
        let guard_refusals = counter(
            "cachic_upstream_guard_refusals_total",
            "Upstream fetches refused by the address guard, by reason",
            &["reason"],
        )?;
        let connections = IntGauge::new("cachic_client_connections", "Open client connections")?;

        for c in [
            Box::new(requests.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(bytes_served.clone()),
            Box::new(bytes_fetched.clone()),
            Box::new(upstream_seconds.clone()),
            Box::new(inflight.clone()),
            Box::new(checksum_failures.clone()),
            Box::new(generation_bumps.clone()),
            Box::new(guard_refusals.clone()),
            Box::new(connections.clone()),
        ] {
            registry.register(c)?;
        }

        // foyer writes into the same registry, so its counters - including the overflow counter
        // that M0 needed and did not have - are scraped alongside ours.
        let foyer_registry = Arc::new(PrometheusMetricsRegistry::new(registry.clone()));

        Ok((
            Self {
                registry,
                requests,
                bytes_served,
                bytes_fetched,
                upstream_seconds,
                inflight,
                checksum_failures,
                generation_bumps,
                guard_refusals,
                connections,
            },
            foyer_registry,
        ))
    }

    /// Render the current values in Prometheus text format.
    pub fn render(&self) -> Result<String, prometheus::Error> {
        let mut buffer = Vec::new();
        TextEncoder::new().encode(&self.registry.gather(), &mut buffer)?;
        String::from_utf8(buffer).map_err(|e| prometheus::Error::Msg(e.to_string()))
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_prometheus_text() {
        let (m, _) = Metrics::new().unwrap();
        m.requests.with_label_values(&["steam", "HIT"]).inc();
        m.bytes_served
            .with_label_values(&["steam", "HIT"])
            .inc_by(4096);
        let text = m.render().unwrap();
        assert!(text.contains("cachic_requests_total"), "{text}");
        assert!(text.contains("service=\"steam\""), "{text}");
        assert!(text.contains("status=\"HIT\""), "{text}");
    }

    #[test]
    fn every_metric_is_registered() {
        // A metric that is recorded but not registered is invisible: the code looks instrumented
        // and the dashboard stays empty.
        let (m, _) = Metrics::new().unwrap();
        m.requests.with_label_values(&["steam", "HIT"]).inc();
        // A label-vec with no observed values renders nothing, so every one must be exercised
        // here or this test cannot tell "unregistered" from "never used".
        m.bytes_served
            .with_label_values(&["steam", "HIT"])
            .inc_by(1);
        m.bytes_fetched.with_label_values(&["steam"]).inc_by(1);
        m.upstream_seconds
            .with_label_values(&["steam"])
            .observe(0.1);
        m.inflight.set(3);
        m.checksum_failures.with_label_values(&["steam"]).inc();
        m.generation_bumps.with_label_values(&["steam"]).inc();
        m.guard_refusals.with_label_values(&["private"]).inc();
        m.connections.set(7);

        let text = m.render().unwrap();
        for name in [
            "cachic_requests_total",
            "cachic_bytes_served_total",
            "cachic_bytes_fetched_total",
            "cachic_upstream_seconds",
            "cachic_upstream_inflight",
            "cachic_checksum_failures_total",
            "cachic_generation_bumps_total",
            "cachic_upstream_guard_refusals_total",
            "cachic_client_connections",
        ] {
            assert!(text.contains(name), "{name} is not exported");
        }
    }

    #[test]
    fn label_sets_are_closed() {
        // The cardinality guarantee. Service names come from cache-domains; statuses are the four
        // X-Cache values. Nothing here takes a URL, a host or a client address.
        let (m, _) = Metrics::new().unwrap();
        for status in CACHE_STATUSES {
            m.requests.with_label_values(&["steam", status]).inc();
        }
        let text = m.render().unwrap();
        // One series per status, and no others.
        let series = text
            .lines()
            .filter(|l| l.starts_with("cachic_requests_total{"))
            .count();
        assert_eq!(series, CACHE_STATUSES.len());
        assert!(
            !text.contains("url=") && !text.contains("path=") && !text.contains("client="),
            "an unbounded label has appeared: {text}"
        );
    }

    #[test]
    fn upstream_latency_buckets_span_lan_to_slow_wan() {
        let (m, _) = Metrics::new().unwrap();
        m.upstream_seconds
            .with_label_values(&["steam"])
            .observe(0.003);
        m.upstream_seconds
            .with_label_values(&["steam"])
            .observe(12.0);
        let text = m.render().unwrap();
        assert!(text.contains("le=\"0.005\""), "no fast bucket: {text}");
        assert!(text.contains("le=\"30\""), "no slow bucket: {text}");
    }
}
