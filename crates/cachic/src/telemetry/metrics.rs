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
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry,
    TextEncoder,
};

/// Every metric cachic records itself.
pub struct Metrics {
    registry: Registry,

    // The label is `cdn_service`, not `service`. kube-prometheus-stack attaches its own
    // `service` label (the Kubernetes Service name) and, on collision, Prometheus keeps its own
    // and renames ours to `exported_service`. Every per-CDN query then groups by the Service name
    // and collapses to a single flat series, which silently destroys exactly the breakdown these
    // metrics exist to provide.
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

    // --- Added after the rc1 deployment, which could not answer "why is this slow", "is it
    // erroring", "which CDN is saturated", "am I serving stale" or "how full is the disk" from
    // metrics alone. Each of these exists because an operator asked one of those questions.
    /// End-to-end service time by service and cache status.
    ///
    /// Distinct from `upstream_seconds`, which times only the origin leg. Without both, a slow
    /// request cannot be attributed to the store, the origin, or writing to the client.
    pub request_seconds: HistogramVec,
    /// Responses by service and HTTP status code. The cache-status label is a different question.
    pub responses: IntCounterVec,
    /// Upstream failures by service and kind, so a timeout is distinguishable from a 5xx.
    pub upstream_errors: IntCounterVec,
    /// Responses served from cache because the origin failed (FR-22).
    pub stale_responses: IntCounterVec,
    /// Slice fetches in flight, per service, against that service's ceiling (FR-09).
    pub upstream_inflight_service: IntGaugeVec,
    /// The per-service ceiling, so saturation is a ratio rather than a number needing context.
    pub upstream_limit_service: IntGaugeVec,
    /// Requests still being served. During a drain this is what shutdown is waiting for.
    pub requests_in_flight: IntGauge,
    /// 1 while draining, 0 otherwise.
    pub draining: IntGauge,
    /// Bytes the disk tier is allowed to use, after the free-space guard clamps the configured
    /// size. Below the configured value means the guard is holding back.
    pub store_capacity_bytes: IntGauge,
    /// Free space on the filesystem backing the cache.
    pub disk_available_bytes: IntGauge,
    /// Total size of that filesystem.
    pub disk_total_bytes: IntGauge,
    /// 1 while the free-space guard is clamping the configured disk size.
    pub disk_guard_engaged: IntGauge,
    /// Objects the index knows about.
    pub index_objects: IntGauge,
    /// Bytes those objects account for. With `store_capacity_bytes`, this is "how full am I".
    pub index_bytes: IntGauge,
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
            &["cdn_service", "status"],
        )?;
        let bytes_served = counter(
            "cachic_bytes_served_total",
            "Bytes served to clients by service and cache status",
            &["cdn_service", "status"],
        )?;
        let bytes_fetched = counter(
            "cachic_bytes_fetched_total",
            "Bytes fetched from origins by service",
            &["cdn_service"],
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
            &["cdn_service"],
        )?;
        let inflight = IntGauge::new(
            "cachic_upstream_inflight",
            "Slice fetches currently in flight",
        )?;
        let checksum_failures = counter(
            "cachic_checksum_failures_total",
            "Slices whose checksum failed verification on read",
            &["cdn_service"],
        )?;
        let generation_bumps = counter(
            "cachic_generation_bumps_total",
            "Objects invalidated because their validators changed",
            &["cdn_service"],
        )?;
        let guard_refusals = counter(
            "cachic_upstream_guard_refusals_total",
            "Upstream fetches refused by the address guard, by reason",
            &["reason"],
        )?;
        let connections = IntGauge::new("cachic_client_connections", "Open client connections")?;

        let request_seconds = HistogramVec::new(
            HistogramOpts::new(
                "cachic_request_seconds",
                "End-to-end request service time in seconds",
            )
            // A cache hit should land in the first few buckets; the long tail is a cold fill of a
            // large range, which is why this reaches further out than the upstream histogram.
            .buckets(vec![
                0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
            ]),
            &["cdn_service", "status"],
        )?;
        let responses = counter(
            "cachic_responses_total",
            "Responses by service and HTTP status code",
            &["cdn_service", "code"],
        )?;
        let upstream_errors = counter(
            "cachic_upstream_errors_total",
            "Upstream failures by service and kind",
            &["cdn_service", "kind"],
        )?;
        let stale_responses = counter(
            "cachic_stale_responses_total",
            "Responses served from cache because the origin failed",
            &["cdn_service"],
        )?;
        let gauge_vec = |name: &str, help: &str, labels: &[&str]| {
            IntGaugeVec::new(Opts::new(name, help), labels)
        };
        let upstream_inflight_service = gauge_vec(
            "cachic_upstream_inflight_service",
            "Slice fetches in flight per service",
            &["cdn_service"],
        )?;
        let upstream_limit_service = gauge_vec(
            "cachic_upstream_limit_service",
            "Configured concurrent upstream fetch ceiling per service",
            &["cdn_service"],
        )?;
        let requests_in_flight = IntGauge::new(
            "cachic_requests_in_flight",
            "Requests currently being served",
        )?;
        let draining = IntGauge::new("cachic_draining", "1 while draining, 0 otherwise")?;
        let store_capacity_bytes = IntGauge::new(
            "cachic_store_capacity_bytes",
            "Disk tier capacity after the free-space guard",
        )?;
        let disk_available_bytes = IntGauge::new(
            "cachic_disk_available_bytes",
            "Free space on the filesystem backing the cache",
        )?;
        let disk_total_bytes = IntGauge::new(
            "cachic_disk_total_bytes",
            "Total size of the filesystem backing the cache",
        )?;
        let disk_guard_engaged = IntGauge::new(
            "cachic_disk_guard_engaged",
            "1 while the free-space guard is clamping the configured disk size",
        )?;
        let index_objects = IntGauge::new("cachic_index_objects", "Objects in the index")?;
        let index_bytes = IntGauge::new(
            "cachic_index_bytes",
            "Bytes accounted for by indexed objects",
        )?;

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
            Box::new(request_seconds.clone()),
            Box::new(responses.clone()),
            Box::new(upstream_errors.clone()),
            Box::new(stale_responses.clone()),
            Box::new(upstream_inflight_service.clone()),
            Box::new(upstream_limit_service.clone()),
            Box::new(requests_in_flight.clone()),
            Box::new(draining.clone()),
            Box::new(store_capacity_bytes.clone()),
            Box::new(disk_available_bytes.clone()),
            Box::new(disk_total_bytes.clone()),
            Box::new(disk_guard_engaged.clone()),
            Box::new(index_objects.clone()),
            Box::new(index_bytes.clone()),
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
                request_seconds,
                responses,
                upstream_errors,
                stale_responses,
                upstream_inflight_service,
                upstream_limit_service,
                requests_in_flight,
                draining,
                store_capacity_bytes,
                disk_available_bytes,
                disk_total_bytes,
                disk_guard_engaged,
                index_objects,
                index_bytes,
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
        assert!(text.contains("cdn_service=\"steam\""), "{text}");
        assert!(text.contains("status=\"HIT\""), "{text}");
    }

    #[test]
    fn no_metric_uses_a_label_prometheus_will_take_for_itself() {
        // `service` is effectively reserved in a Kubernetes monitoring stack: the
        // kube-prometheus-stack ServiceMonitor pipeline attaches its own `service` label holding
        // the Kubernetes Service name, and on collision Prometheus keeps its own and renames the
        // exporter's to `exported_service`. Every per-CDN query then groups by the Service name
        // and silently collapses to one flat series.
        //
        // Substring matching would not catch this - `cdn_service="steam"` contains
        // `service="steam"` - so this parses the label names out and compares them exactly.
        let (m, _) = Metrics::new().unwrap();
        m.requests.with_label_values(&["steam", "HIT"]).inc();
        m.bytes_fetched.with_label_values(&["steam"]).inc_by(1);
        m.upstream_seconds
            .with_label_values(&["steam"])
            .observe(0.1);
        let text = m.render().unwrap();

        const RESERVED: &[&str] = &[
            "service",
            "job",
            "instance",
            "pod",
            "namespace",
            "container",
        ];
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let Some(labels) = line
                .split_once('{')
                .and_then(|(_, rest)| rest.rsplit_once('}'))
                .map(|(labels, _)| labels)
            else {
                continue;
            };
            for pair in labels.split(',') {
                let name = pair.split('=').next().unwrap_or("").trim();
                assert!(
                    !RESERVED.contains(&name),
                    "metric label {name:?} collides with one Prometheus attaches itself, \
                     so the exporter's value is renamed to exported_{name} and every query \
                     grouping by it breaks: {line}"
                );
            }
        }
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
        m.request_seconds
            .with_label_values(&["steam", "HIT"])
            .observe(0.01);
        m.responses.with_label_values(&["steam", "206"]).inc();
        m.upstream_errors
            .with_label_values(&["steam", "timeout"])
            .inc();
        m.stale_responses.with_label_values(&["steam"]).inc();
        m.upstream_inflight_service
            .with_label_values(&["steam"])
            .set(2);
        m.upstream_limit_service
            .with_label_values(&["steam"])
            .set(8);
        m.requests_in_flight.set(5);
        m.draining.set(0);
        m.store_capacity_bytes.set(1);
        m.disk_available_bytes.set(1);
        m.disk_total_bytes.set(1);
        m.disk_guard_engaged.set(0);
        m.index_objects.set(1);
        m.index_bytes.set(1);

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
            "cachic_request_seconds",
            "cachic_responses_total",
            "cachic_upstream_errors_total",
            "cachic_stale_responses_total",
            "cachic_upstream_inflight_service",
            "cachic_upstream_limit_service",
            "cachic_requests_in_flight",
            "cachic_draining",
            "cachic_store_capacity_bytes",
            "cachic_disk_available_bytes",
            "cachic_disk_total_bytes",
            "cachic_disk_guard_engaged",
            "cachic_index_objects",
            "cachic_index_bytes",
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
