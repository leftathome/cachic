//! Every declared metric must have something that writes it.
//!
//! `every_metric_is_registered` proves a metric is *exported*, and it does that by exercising each
//! one by hand inside the test. That is exactly why it could pass while six metrics were declared,
//! registered, documented, drawn on the dashboard, and never written by any code path:
//! `bytes_fetched`, `upstream_seconds`, `inflight`, `checksum_failures`, `generation_bumps` and
//! `guard_refusals`.
//!
//! The consequence was not merely empty panels. `docs/metrics.md` calls
//! `cachic_checksum_failures_total` the corruption alarm and `docs/rc-test-plan.md` makes "exactly
//! zero" a soak pass criterion - a criterion nothing could ever falsify, which is worse than
//! having no criterion at all.
//!
//! This scans the source instead of the exported text, because the question is about the code, not
//! about a registry that a test has just finished populating itself.

use std::{collections::BTreeSet, fs, path::Path};

/// Fields on `Metrics` that are deliberately not written anywhere, with the reason.
///
/// Empty, and it should stay that way. A metric nothing writes is either unfinished work or a
/// promise the code does not keep.
const EXEMPT: &[(&str, &str)] = &[];

fn read_all_rust(dir: &Path, skip: &str, out: &mut String) {
    for entry in fs::read_dir(dir).expect("cannot read source directory") {
        let path = entry.expect("bad directory entry").path();
        if path.is_dir() {
            read_all_rust(&path, skip, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            && !path.to_string_lossy().ends_with(skip)
        {
            out.push_str(&fs::read_to_string(&path).expect("cannot read source file"));
            out.push('\n');
        }
    }
}

/// The `pub name: Type,` fields of the `Metrics` struct.
fn declared_metrics(source: &str) -> BTreeSet<String> {
    let start = source
        .find("pub struct Metrics {")
        .expect("the Metrics struct moved; this test needs updating");
    let body = &source[start..];
    let end = body.find("\n}").expect("unterminated Metrics struct");
    body[..end]
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("pub ")?;
            let (name, _) = rest.split_once(':')?;
            let name = name.trim();
            // `registry` is the Registry itself, not a metric.
            (name != "registry" && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
                .then(|| name.to_string())
        })
        .collect()
}

#[test]
fn every_declared_metric_is_written_somewhere() {
    let metrics_rs = Path::new("src/telemetry/metrics.rs");
    let declared = declared_metrics(&fs::read_to_string(metrics_rs).expect("metrics.rs"));
    assert!(
        declared.len() > 10,
        "only {} metrics were parsed out of the struct; the parser has broken and this test \
         would pass vacuously",
        declared.len()
    );

    let mut source = String::new();
    read_all_rust(Path::new("src"), "telemetry/metrics.rs", &mut source);
    // Collapse whitespace so a multi-line builder chain reads as one line.
    let flat: String = source.split_whitespace().collect::<Vec<_>>().join(" ");

    // A write is a *mutation*. `with_label_values` only selects a child metric and changes
    // nothing, so counting it as a write is how the first version of this test passed after the
    // `.inc()` was deleted from under it - the same failure mode it exists to catch.
    const MUTATIONS: &[&str] = &[
        ".inc()",
        ".inc_by",
        ".dec()",
        ".set(",
        ".observe(",
        ".reset()",
    ];

    let exempt: BTreeSet<&str> = EXEMPT.iter().map(|(name, _)| *name).collect();
    let mut dead = Vec::new();
    for metric in &declared {
        if exempt.contains(metric.as_str()) {
            continue;
        }
        // Look from each mention of the field to the end of its statement, so a builder chain
        // spanning several lines is judged as a whole.
        let needle = format!(".{metric}");
        let mut written = false;
        let mut from = 0;
        while let Some(at) = flat[from..].find(&needle) {
            let start = from + at;
            // Reject a longer field that merely starts with this name.
            let after = flat[start + needle.len()..].chars().next();
            if after.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                from = start + needle.len();
                continue;
            }
            let statement_end = flat[start..]
                .find(';')
                .map(|i| start + i)
                .unwrap_or(flat.len());
            let statement = &flat[start..statement_end];
            if MUTATIONS.iter().any(|m| statement.contains(m)) {
                written = true;
                break;
            }
            from = start + needle.len();
        }
        if !written {
            dead.push(metric.clone());
        }
    }

    assert!(
        dead.is_empty(),
        "these metrics are declared and registered but nothing writes them, so their panels are \
         permanently empty and any alert on them can never fire: {dead:?}"
    );
}
