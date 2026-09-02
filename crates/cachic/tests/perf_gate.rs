//! Performance gate.
//!
//! The project's floor standard is "as good and fast as nginx, but easier to configure and
//! operate". The ideal is to be provably faster. Neither is worth anything as an aspiration in a
//! README, so it is enforced here.
//!
//! Two thresholds:
//!
//! - **Floor** - hard failure. Below this we have broken the floor standard and the build stops.
//! - **Target** - loud warning. Above the floor but below where this hardware should reach, which
//!   means something regressed even though it is not yet fatal.
//!
//! Both are overridable by environment variable so a dedicated benchmark host can enforce the real
//! numbers while a shared CI runner enforces only a catastrophic-regression backstop.
//!
//! ## On the floor number
//!
//! The floor *should* be nginx's throughput on the same hardware, measured in the same run. That
//! comparison is TASK-25, which runs `lancachenet/monolithic` against the same data volume. Until
//! it exists, `DEFAULT_FLOOR_GBPS` is a provisional backstop chosen to sit well below the observed
//! noise band, so it catches a catastrophic regression anywhere without flaking on slow machines.
//! **When TASK-25 lands, replace it with the measured nginx figure.**
//!
//! ## On noise
//!
//! Measured on the development host, throughput varies 2.74-3.68 Gbps between runs (about 34%)
//! while varying under 2% within a run. That is contention from other work on the box. Throughput
//! noise is one-sided - interference can only make you slower - so the gate takes the best of
//! several rounds rather than the mean. A mean would encode whatever else the machine was doing.
//!
//! ## Release builds only
//!
//! The test profile is unoptimised, and it shows: 2.16 Gbps debug against 2.57 Gbps release on the
//! same machine. A gate enforced against a debug build would warn on every run and be deleted
//! within a week, so in debug it measures and reports but does not enforce. Run `just perf`.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use cachic::spike::{
    proxy::{SpikeConfig, SpikeProxy},
    store::StoreConfig,
};
use cachic_testkit::mockcdn::{Config as CdnConfig, MockCdn};
use futures_util::StreamExt;

/// Hard failure below this. Provisional until TASK-25 measures nginx on the same hardware.
const DEFAULT_FLOOR_GBPS: f64 = 1.0;

/// Warn below this. What the development hardware should comfortably reach.
const DEFAULT_TARGET_GBPS: f64 = 2.5;

const CLIENTS: usize = 8;
const OBJECT_MIB: u64 = 128;
const ROUNDS: usize = 3;

fn threshold(var: &str, default: f64) -> f64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        let base = std::env::var("CACHIC_TEST_TMP").unwrap_or_else(|_| "/tmp".into());
        let path = std::path::Path::new(&base).join(format!(
            "cachic-perf-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Warm-cache hit throughput in Gbps: what a client sees reading cached content, which is the
/// number the floor standard is about.
#[tokio::test(flavor = "multi_thread")]
async fn warm_hit_throughput_meets_the_floor_standard() {
    let floor = threshold("CACHIC_PERF_FLOOR_GBPS", DEFAULT_FLOOR_GBPS);
    let target = threshold("CACHIC_PERF_TARGET_GBPS", DEFAULT_TARGET_GBPS);
    // Debug builds measure roughly 20% low, so enforcing there would cry wolf on every run.
    let enforce = !cfg!(debug_assertions);

    let origin = MockCdn::start(CdnConfig::default()).await.unwrap();
    let scratch = Scratch::new();
    let size = OBJECT_MIB * 1024 * 1024;

    let mut config = SpikeConfig::new(origin.base_url(), &scratch.0);
    config.slice_size = 1024 * 1024;
    config.readahead = 8;
    config.store = StoreConfig {
        // Memory tier comfortably larger than the object, so this measures the serving path
        // rather than the disk.
        memory_bytes: (size as usize) * 2,
        disk_bytes: (size as usize) * 4,
        block_bytes: 32 * 1024 * 1024,
        direct_io: false,
    };
    let proxy = Arc::new(SpikeProxy::start(config).await.unwrap());
    let url = format!(
        "{}{}",
        proxy.base_url(),
        MockCdn::object_path("perf-gate", size)
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap();

    // Warm the cache. Everything after this is a hit.
    let warm = client.get(&url).send().await.unwrap();
    assert_eq!(warm.status(), 200);
    assert_eq!(warm.bytes().await.unwrap().len() as u64, size);

    let mut best_gbps = 0.0f64;
    for round in 0..ROUNDS {
        let read = Arc::new(AtomicU64::new(0));
        let start = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..CLIENTS {
            let client = client.clone();
            let url = url.clone();
            let read = read.clone();
            handles.push(tokio::spawn(async move {
                let response = client.get(url).send().await.unwrap();
                assert_eq!(response.status(), 200);
                assert_eq!(
                    response.headers()["x-cache"],
                    "HIT",
                    "perf gate must measure hits, not fills"
                );
                let mut stream = response.bytes_stream();
                let mut n = 0u64;
                while let Some(chunk) = stream.next().await {
                    n += chunk.unwrap().len() as u64;
                }
                read.fetch_add(n, Ordering::Relaxed);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let elapsed = start.elapsed();
        let total = read.load(Ordering::Relaxed);
        assert_eq!(total, size * CLIENTS as u64, "short read in round {round}");

        let gbps = (total as f64 * 8.0) / elapsed.as_secs_f64() / 1e9;
        eprintln!("perf gate: round {round} = {gbps:.2} Gbps");
        best_gbps = best_gbps.max(gbps);
    }

    proxy.close().await.unwrap();

    // Always print the number, pass or fail, so a trend is visible in build logs.
    eprintln!(
        "perf gate: best of {ROUNDS} rounds = {best_gbps:.2} Gbps \
         ({CLIENTS} clients, {OBJECT_MIB} MiB object, warm cache) \
         [floor {floor:.2}, target {target:.2}]"
    );

    if !enforce {
        eprintln!(
            "perf gate: debug build, thresholds not enforced (debug measures ~20% low). \
             Run `just perf` for the real gate."
        );
        return;
    }

    assert!(
        best_gbps >= floor,
        "FLOOR BREACHED: warm hit throughput {best_gbps:.2} Gbps is below the floor of \
         {floor:.2} Gbps.\n\
         The floor standard is 'as good and fast as nginx, but easier to configure and \
         operate'. Falling under it means the project has lost its reason to exist, so this \
         is a hard failure rather than a regression to triage later.\n\
         If this machine is genuinely slower than the floor assumes, set \
         CACHIC_PERF_FLOOR_GBPS deliberately for this host - do not delete the assertion."
    );

    if best_gbps < target {
        eprintln!(
            "\n!! PERF WARNING: {best_gbps:.2} Gbps is below the {target:.2} Gbps target.\n\
             !! Above the floor, so not fatal, but this hardware should reach the target.\n\
             !! Investigate before it becomes a floor breach.\n"
        );
    }
}
