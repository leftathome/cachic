//! Benchmark harness implementing the scenarios in plan section 9 (TASK-25).
//!
//! Emits CSV so results can be committed alongside the exact command that produced them.
//!
//! # What this can and cannot do
//!
//! It drives cachic. It does **not** drive `lancachenet/monolithic`, because the parity claim
//! requires both engines against the same data volume on the same hardware in alternating runs,
//! and that is an orchestration job for the benchmark host rather than something this binary
//! should pretend to do. `docs/benchmarks/README.md` carries the protocol.
//!
//! Every number is only valid for the hardware it was taken on, and the harness records that
//! hardware alongside the numbers so a committed result cannot be read out of context.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use cachic::{
    orchestrator::Orchestrator,
    proxy::server::{Server, ServerConfig},
    services::{domains::DomainList, matcher::Matcher},
    store::{
        hybrid::{SliceStore, StoreConfig},
        index::ObjectIndex,
    },
    upstream::{
        client::{ClientConfig, UpstreamClient},
        resolver::UpstreamResolver,
    },
};
use cachic_testkit::{
    loadgen::Collector,
    mockcdn::{Config as CdnConfig, MockCdn},
};
use clap::{Parser, ValueEnum};
use futures_util::StreamExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Scenario {
    /// S1: warm, single client, one large object.
    S1,
    /// S2: warm, N clients, the same object.
    S2,
    /// S3: warm, N clients, N distinct objects.
    S3,
    /// S4: cold fill, N clients, the same object. Measures upstream amplification.
    S4,
    /// S5: random ranges into large objects, the Windows Update shape.
    S5,
    /// S6: restart with a populated cache. Time to first hit.
    S6,
    /// All of the above.
    All,
}

#[derive(Parser, Debug)]
#[command(name = "bench", about = "cachic benchmark harness (plan section 9)")]
struct Args {
    #[arg(long, value_enum, default_value = "all")]
    scenario: Scenario,

    #[arg(long, default_value_t = 32)]
    clients: usize,

    /// Object size in MiB. The protocol calls for 20 GB objects on the benchmark host; the
    /// default here is small enough to run on a laptop.
    #[arg(long, default_value_t = 256)]
    object_mib: u64,

    #[arg(long, default_value_t = 1)]
    slice_mib: u64,

    #[arg(long, default_value_t = 2048)]
    mem_mib: usize,

    #[arg(long, default_value_t = 32768)]
    disk_mib: usize,

    #[arg(long, default_value = "/var/tmp/cachic-bench")]
    dir: std::path::PathBuf,

    /// Emulated origin latency, standing in for a WAN link.
    #[arg(long, default_value_t = 0)]
    origin_delay_ms: u64,
}

fn row(scenario: &str, metric: &str, value: impl std::fmt::Display, unit: &str, notes: &str) {
    // Commas in a note would silently shift every column to its right, and a benchmark result
    // that parses into the wrong columns is worse than one that fails to parse.
    println!(
        "{scenario},{metric},{value},{unit},{}",
        notes.replace(',', ";")
    );
}

struct Rig {
    origin: MockCdn,
    server: Server,
    dir: std::path::PathBuf,
}

impl Rig {
    async fn start(args: &Args, fresh: bool) -> Self {
        let dir = args.dir.clone();
        if fresh {
            let _ = std::fs::remove_dir_all(&dir);
        }
        std::fs::create_dir_all(&dir).unwrap();

        let origin = MockCdn::start(CdnConfig {
            first_byte_delay: (args.origin_delay_ms > 0)
                .then(|| Duration::from_millis(args.origin_delay_ms)),
            ..CdnConfig::default()
        })
        .await
        .unwrap();

        let store = SliceStore::open(
            &dir.join("slices"),
            &StoreConfig {
                memory_bytes: args.mem_mib * 1024 * 1024,
                disk_bytes: args.disk_mib * 1024 * 1024,
                ..StoreConfig::default()
            },
        )
        .await
        .unwrap();
        let index = Arc::new(ObjectIndex::open(&dir.join("index.redb")).unwrap());
        let resolver =
            Arc::new(UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], true).unwrap());
        let upstream = UpstreamClient::new(resolver, ClientConfig::default()).unwrap();
        let orchestrator = Arc::new(Orchestrator::new(
            store,
            index,
            upstream,
            (args.slice_mib * 1024 * 1024) as u32,
            8,
        ));

        let host = origin.addr().ip().to_string();
        let mut files = std::collections::BTreeMap::new();
        files.insert("m.txt".to_string(), format!("{host}\n"));
        let matcher = Arc::new(Matcher::build(
            &DomainList::parse(
                r#"{"cache_domains":[{"name":"bench","domain_files":["m.txt"]}]}"#,
                &files,
            )
            .unwrap(),
        ));

        let server = Server::bind(
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(ServerConfig::with_defaults(orchestrator, matcher, "bench")),
        )
        .await
        .unwrap();

        Self {
            origin,
            server,
            dir,
        }
    }

    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(600))
            .build()
            .unwrap()
    }

    fn url(&self, name: &str, size: u64) -> String {
        format!(
            "{}{}",
            self.server.base_url(),
            MockCdn::object_path(name, size)
        )
    }

    fn host(&self) -> String {
        self.origin.addr().to_string()
    }
}

/// Drive `clients` concurrent readers and report throughput and TTFB.
async fn drive(
    rig: &Rig,
    urls: Vec<String>,
    clients: usize,
    range: Option<String>,
) -> cachic_testkit::loadgen::Report {
    let collector = Arc::new(std::sync::Mutex::new(Collector::new()));
    let bytes = Arc::new(AtomicU64::new(0));
    let start = Instant::now();

    let mut handles = Vec::new();
    for i in 0..clients {
        let client = rig.client();
        let url = urls[i % urls.len()].clone();
        let host = rig.host();
        let range = range.clone();
        let collector = collector.clone();
        let bytes = bytes.clone();
        handles.push(tokio::spawn(async move {
            let t0 = Instant::now();
            let mut request = client.get(url).header("host", host);
            if let Some(range) = range {
                request = request.header("range", range);
            }
            let response = request.send().await.unwrap();
            let mut stream = response.bytes_stream();
            let mut first = true;
            let mut n = 0u64;
            let mut ttfb = Duration::ZERO;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.unwrap();
                if first {
                    ttfb = t0.elapsed();
                    first = false;
                }
                n += chunk.len() as u64;
            }
            bytes.fetch_add(n, Ordering::Relaxed);
            collector.lock().unwrap().record(n, ttfb);
        }));
    }
    for handle in handles {
        handle.await.unwrap();
    }

    let elapsed = start.elapsed();
    let collector = Arc::try_unwrap(collector)
        .ok()
        .unwrap()
        .into_inner()
        .unwrap();
    collector.finish(elapsed)
}

fn report(scenario: &str, r: &cachic_testkit::loadgen::Report, notes: &str) {
    row(
        scenario,
        "throughput",
        format!("{:.2}", r.gbps()),
        "Gbps",
        notes,
    );
    row(
        scenario,
        "throughput",
        format!("{:.0}", r.mib_per_second()),
        "MiB/s",
        "",
    );
    row(
        scenario,
        "ttfb_p50",
        format!("{:.2}", r.ttfb_percentile(0.50).as_secs_f64() * 1000.0),
        "ms",
        "",
    );
    row(
        scenario,
        "ttfb_p99",
        format!("{:.2}", r.ttfb_percentile(0.99).as_secs_f64() * 1000.0),
        "ms",
        "",
    );
    row(scenario, "requests", r.requests, "count", "");
}

async fn warm(rig: &Rig, name: &str, size: u64) {
    let response = rig
        .client()
        .get(rig.url(name, size))
        .header("host", rig.host())
        .send()
        .await
        .unwrap();
    let _ = response.bytes().await.unwrap();
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let size = args.object_mib * 1024 * 1024;

    println!("scenario,metric,value,unit,notes");
    row(
        "environment",
        "hardware",
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|v| v.trim().replace(',', ";"))
            })
            .unwrap_or_else(|| "unknown".into()),
        "",
        "every number below is only valid for this hardware",
    );
    row("environment", "object_size", args.object_mib, "MiB", "");
    row("environment", "slice_size", args.slice_mib, "MiB", "");
    row("environment", "clients", args.clients, "count", "");
    row("environment", "memory_tier", args.mem_mib, "MiB", "");

    let run = |s: Scenario| args.scenario == s || args.scenario == Scenario::All;

    if run(Scenario::S1) {
        let rig = Rig::start(&args, true).await;
        warm(&rig, "s1", size).await;
        let r = drive(&rig, vec![rig.url("s1", size)], 1, None).await;
        report("S1", &r, "warm, single client, whole object");
    }

    if run(Scenario::S2) {
        let rig = Rig::start(&args, true).await;
        warm(&rig, "s2", size).await;
        let r = drive(&rig, vec![rig.url("s2", size)], args.clients, None).await;
        report("S2", &r, "warm, N clients, same object");
    }

    if run(Scenario::S3) {
        let rig = Rig::start(&args, true).await;
        let names: Vec<String> = (0..args.clients).map(|i| format!("s3-{i}")).collect();
        for name in &names {
            warm(&rig, name, size).await;
        }
        let urls = names.iter().map(|n| rig.url(n, size)).collect();
        let r = drive(&rig, urls, args.clients, None).await;
        report("S3", &r, "warm, N clients, N distinct objects");
    }

    if run(Scenario::S4) {
        // The scenario that matters most: a cold object hit by many clients at once must produce
        // approximately one upstream fetch per slice, not one per client.
        let rig = Rig::start(&args, true).await;
        let r = drive(&rig, vec![rig.url("s4", size)], args.clients, None).await;
        report("S4", &r, "cold fill, N clients, same object");
        let upstream_bytes = rig.origin.stats().bytes_served();
        row("S4", "upstream_bytes", upstream_bytes, "bytes", "");
        row(
            "S4",
            "upstream_amplification",
            format!("{:.2}", upstream_bytes as f64 / size as f64),
            "ratio",
            "1.0 is perfect coalescing; N would be none at all",
        );
    }

    if run(Scenario::S5) {
        // Random ranges, the Windows Update and Blizzard shape.
        let rig = Rig::start(&args, true).await;
        warm(&rig, "s5", size).await;
        let start = size / 4;
        let end = start + (8 * 1024 * 1024) - 1;
        let r = drive(
            &rig,
            vec![rig.url("s5", size)],
            args.clients,
            Some(format!("bytes={start}-{end}")),
        )
        .await;
        report("S5", &r, "warm, N clients, 8 MiB ranges");
    }

    if run(Scenario::S6) {
        // Restart with a populated cache: how quickly does it serve again?
        let rig = Rig::start(&args, true).await;
        warm(&rig, "s6", size).await;
        drop(rig);
        tokio::time::sleep(Duration::from_millis(500)).await;

        let start = Instant::now();
        let rig = Rig::start(&args, false).await;
        let open = start.elapsed();
        row(
            "S6",
            "store_open",
            format!("{:.3}", open.as_secs_f64()),
            "s",
            "reopen a populated cache",
        );

        let start = Instant::now();
        let r = drive(
            &rig,
            vec![rig.url("s6", size)],
            1,
            Some("bytes=0-1048575".into()),
        )
        .await;
        row(
            "S6",
            "time_to_first_hit",
            format!("{:.3}", start.elapsed().as_secs_f64() * 1000.0),
            "ms",
            "",
        );
        report("S6", &r, "after restart");
        let _ = std::fs::remove_dir_all(&rig.dir);
    }
}
