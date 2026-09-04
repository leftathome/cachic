//! Periodic sampling of state that is not event-driven.
//!
//! Counters and histograms are recorded where the event happens. These are levels - free disk,
//! index occupancy, in-flight requests, per-service saturation - and nothing "happens" when they
//! change, so something has to look. The rc1 deployment had all of this computed and reachable
//! only through the admin API, which meant it was absent from dashboards and from any alert.

use std::{sync::Arc, time::Duration};

use crate::{
    proxy::shutdown::Drain,
    store::{index::ObjectIndex, space},
    telemetry::metrics::Metrics,
};

/// Reports permits still available for a service, or `None` if it has no ceiling.
pub type ServicePermits = Arc<dyn Fn(&str) -> Option<usize> + Send + Sync>;

pub struct Inputs {
    pub metrics: Arc<Metrics>,
    pub index: Arc<ObjectIndex>,
    pub drain: Arc<Drain>,
    pub data_dir: std::path::PathBuf,
    pub configured_disk_bytes: u64,
    pub min_free_bytes: u64,
    /// Per-service upstream ceilings, so saturation can be read as a ratio (FR-09).
    pub per_service_limits: std::collections::BTreeMap<String, usize>,
    pub service_permits: ServicePermits,
}

/// Sample every `interval` until the process exits.
///
/// Frequent enough that a 15-second scrape sees fresh values, cheap enough to ignore: a statfs,
/// two index reads and a handful of atomics.
pub fn spawn(inputs: Inputs, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            sample(&inputs);
        }
    });
}

fn sample(i: &Inputs) {
    let m = &i.metrics;

    m.requests_in_flight.set(i.drain.inflight() as i64);
    m.draining.set(i.drain.is_draining() as i64);

    let indexed_bytes = i.index.total_bytes().unwrap_or(0);
    m.index_objects.set(i.index.len().unwrap_or(0) as i64);
    m.index_bytes.set(indexed_bytes as i64);

    if let Ok(disk) = space::read(&i.data_dir) {
        let effective = space::effective_cap(
            i.configured_disk_bytes,
            i.min_free_bytes,
            indexed_bytes,
            disk,
        );
        m.disk_total_bytes.set(disk.total as i64);
        m.disk_available_bytes.set(disk.available as i64);
        m.store_capacity_bytes.set(effective as i64);
        m.disk_guard_engaged
            .set(space::is_engaged(i.configured_disk_bytes, effective) as i64);
    }

    for (service, limit) in &i.per_service_limits {
        m.upstream_limit_service
            .with_label_values(&[service])
            .set(*limit as i64);
        if let Some(available) = (i.service_permits)(service) {
            m.upstream_inflight_service
                .with_label_values(&[service])
                .set(limit.saturating_sub(available) as i64);
        }
    }
}
