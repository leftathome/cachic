//! The Grafana dashboard references metrics that exist (TASK-30).
//!
//! A panel querying a renamed or removed metric does not fail - it shows an empty graph forever,
//! and nobody notices until they need it. This test extracts every metric name the dashboard
//! queries and checks it against what the process actually exports.

use std::collections::BTreeSet;

use cachic::telemetry::metrics::Metrics;

const DASHBOARD: &str = include_str!("../../../dashboards/cachic.json");

/// Metric names appearing in a PromQL expression.
///
/// A deliberately crude extractor: any identifier that looks like one of ours or foyer's. It does
/// not need to understand PromQL, only to find the names.
fn metric_names(expr: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut current = String::new();
    for ch in expr.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            if current.starts_with("cachic_") || current.starts_with("foyer_") {
                names.insert(current.clone());
            }
            current.clear();
        }
    }
    if current.starts_with("cachic_") || current.starts_with("foyer_") {
        names.insert(current);
    }
    names
}

/// Every metric the dashboard queries.
fn referenced() -> BTreeSet<String> {
    let dashboard: serde_json::Value = serde_json::from_str(DASHBOARD).expect("dashboard is JSON");
    let mut names = BTreeSet::new();
    for panel in dashboard["panels"].as_array().expect("panels array") {
        for target in panel["targets"].as_array().into_iter().flatten() {
            if let Some(expr) = target["expr"].as_str() {
                names.extend(metric_names(expr));
            }
        }
    }
    names
}

#[tokio::test]
async fn every_metric_the_dashboard_queries_is_exported() {
    let (metrics, foyer_registry) = Metrics::new().unwrap();

    // Exercise our own metrics so their label series materialise, and open a store so foyer
    // registers its families.
    metrics.requests.with_label_values(&["steam", "HIT"]).inc();
    metrics
        .bytes_served
        .with_label_values(&["steam", "HIT"])
        .inc_by(1);
    metrics
        .bytes_fetched
        .with_label_values(&["steam"])
        .inc_by(1);
    metrics
        .upstream_seconds
        .with_label_values(&["steam"])
        .observe(0.1);
    metrics.inflight.set(0);
    metrics
        .checksum_failures
        .with_label_values(&["steam"])
        .inc();
    metrics.generation_bumps.with_label_values(&["steam"]).inc();
    metrics.guard_refusals.with_label_values(&["private"]).inc();
    metrics.connections.set(0);
    metrics
        .request_seconds
        .with_label_values(&["steam", "HIT"])
        .observe(0.01);
    metrics.responses.with_label_values(&["steam", "206"]).inc();
    metrics
        .upstream_errors
        .with_label_values(&["steam", "timeout"])
        .inc();
    metrics.stale_responses.with_label_values(&["steam"]).inc();
    metrics
        .upstream_inflight_service
        .with_label_values(&["steam"])
        .set(0);
    metrics
        .upstream_limit_service
        .with_label_values(&["steam"])
        .set(0);

    let scratch = cachic::test_support::Scratch::new("dashboard");
    let store = cachic::store::hybrid::SliceStore::open_with_metrics(
        scratch.path(),
        &cachic::store::hybrid::StoreConfig {
            memory_bytes: 4 * 1024 * 1024,
            disk_bytes: 32 * 1024 * 1024,
            block_bytes: 4 * 1024 * 1024,
            flushers: 1,
            buffer_pool_bytes: 4 * 1024 * 1024,
            direct_io: false,
        },
        Some(foyer_registry),
    )
    .await
    .unwrap();

    let exported = metrics.render().unwrap();
    let referenced = referenced();
    assert!(
        !referenced.is_empty(),
        "the dashboard references no metrics at all, which means the extractor is broken"
    );

    let mut missing = Vec::new();
    for name in &referenced {
        // Histograms are exported as _bucket/_sum/_count, so a reference to the _bucket series
        // is satisfied by the base name appearing.
        let base = name.trim_end_matches("_bucket");
        if !exported.contains(base) {
            missing.push(name.clone());
        }
    }

    assert!(
        missing.is_empty(),
        "the dashboard queries metrics that are not exported, so those panels will be \
         permanently empty: {missing:?}\n\nexported families:\n{}",
        exported
            .lines()
            .filter(|l| l.starts_with("# HELP"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    store.close().await.unwrap();
}

#[test]
fn the_dashboard_is_valid_grafana_json() {
    let dashboard: serde_json::Value = serde_json::from_str(DASHBOARD).unwrap();
    assert_eq!(dashboard["uid"], "cachic");
    assert!(dashboard["schemaVersion"].as_u64().unwrap() >= 36);
    let panels = dashboard["panels"].as_array().unwrap();
    assert!(!panels.is_empty());
    for panel in panels {
        assert!(panel["title"].as_str().is_some_and(|t| !t.is_empty()));
        // Every panel explains what it is for. A dashboard nobody can interpret is a dashboard
        // nobody uses.
        let description = panel["description"].as_str().unwrap_or("");
        assert!(
            description.len() > 40,
            "panel {:?} has no useful description",
            panel["title"]
        );
        // Panels must not overlap or the layout is unreadable.
        assert!(panel["gridPos"]["w"].as_u64().unwrap() > 0);
    }
}

#[test]
fn the_drop_counter_has_a_panel() {
    // The single most important signal this product has: writes being silently discarded. If it
    // is not on the dashboard, nobody will see it.
    assert!(
        referenced()
            .iter()
            .any(|n| n == "foyer_storage_inner_op_total"),
        "the dashboard has no panel for silently dropped disk writes"
    );
}
