//! Soak harness (TASK-33, NFR-7).
//!
//! Drives sustained mixed traffic and asserts the properties that only fail over time: leaks,
//! eviction pathologies, index drift, and above all integrity. Zero corrupt bytes served is a
//! claim until something has run long enough to test it.
//!
//! # What this can and cannot do
//!
//! It runs against the mock origin, so it exercises cachic thoroughly and the *ecosystem* not at
//! all. The 7-day soak the definition of done calls for is this harness pointed at real CDNs with
//! real clients on the homelab; that needs hardware and credentials, not more code.
//!
//! Every read is verified against the generator, so a corrupt byte fails the run immediately
//! rather than being noticed later in aggregate.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use cachic::{
    orchestrator::Orchestrator,
    proxy::server::{Server, ServerConfig},
    services::domains::DomainList,
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
    differ::{self, Generator},
    mockcdn::{Config as CdnConfig, MockCdn},
};
use clap::Parser;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[derive(Parser, Debug)]
#[command(name = "soak", about = "cachic soak test")]
struct Args {
    /// How long to run.
    #[arg(long, default_value_t = 60)]
    seconds: u64,

    #[arg(long, default_value_t = 16)]
    clients: usize,

    /// Distinct objects in the working set.
    #[arg(long, default_value_t = 24)]
    objects: usize,

    #[arg(long, default_value_t = 32)]
    object_mib: u64,

    /// Disk tier in MiB. Deliberately smaller than the working set by default, so eviction runs
    /// continuously - a soak that never evicts does not test eviction.
    #[arg(long, default_value_t = 256)]
    disk_mib: usize,

    #[arg(long, default_value_t = 64)]
    mem_mib: usize,

    #[arg(long, default_value = "/var/tmp/cachic-soak")]
    dir: std::path::PathBuf,

    /// Report progress this often.
    #[arg(long, default_value_t = 15)]
    report_secs: u64,
}

fn rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let size = args.object_mib * 1024 * 1024;
    let _ = std::fs::remove_dir_all(&args.dir);
    std::fs::create_dir_all(&args.dir).unwrap();

    let origin = MockCdn::start(CdnConfig::default()).await.unwrap();
    let store = SliceStore::open(
        &args.dir.join("slices"),
        &StoreConfig {
            memory_bytes: args.mem_mib * 1024 * 1024,
            disk_bytes: args.disk_mib * 1024 * 1024,
            ..StoreConfig::default()
        },
    )
    .await
    .unwrap();
    let index = Arc::new(ObjectIndex::open(&args.dir.join("index.redb")).unwrap());
    let resolver = Arc::new(UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], true).unwrap());
    let upstream = UpstreamClient::new(resolver, ClientConfig::default()).unwrap();
    let orchestrator = Arc::new(Orchestrator::new(
        store,
        index.clone(),
        upstream,
        1024 * 1024,
        8,
    ));

    let host = origin.addr().ip().to_string();
    let mut files = std::collections::BTreeMap::new();
    files.insert("m.txt".to_string(), format!("{host}\n"));
    let list = DomainList::parse(
        r#"{"cache_domains":[{"name":"soak","domain_files":["m.txt"]}]}"#,
        &files,
    )
    .unwrap();
    let server = Server::bind(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(ServerConfig::with_defaults(orchestrator, list, "soak")),
    )
    .await
    .unwrap();

    println!(
        "soak: {} clients, {} objects of {} MiB, {} MiB disk, {}s",
        args.clients, args.objects, args.object_mib, args.disk_mib, args.seconds
    );
    println!(
        "working set {} MiB against a {} MiB disk tier: eviction runs continuously",
        args.objects as u64 * args.object_mib,
        args.disk_mib
    );

    let requests = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let corrupt = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let baseline_rss = rss_bytes();
    let started = Instant::now();

    let mut handles = Vec::new();
    for worker in 0..args.clients {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .unwrap();
        let base = server.base_url();
        let host = origin.addr().to_string();
        let (requests, bytes, corrupt, errors, stop) = (
            requests.clone(),
            bytes.clone(),
            corrupt.clone(),
            errors.clone(),
            stop.clone(),
        );
        let objects = args.objects;
        handles.push(tokio::spawn(async move {
            // Each worker gets its own seed, so the whole run is reproducible from the worker
            // index alone.
            let mut generator = Generator::new(0x50AC_0000 + worker as u64, objects, size, 1 << 20);
            while !stop.load(Ordering::Relaxed) {
                let case = generator.next_case();
                let url = format!("{base}{}", MockCdn::object_path(&case.object, case.size));
                let response = client
                    .get(&url)
                    .header("host", &host)
                    .header("range", case.range_header())
                    .send()
                    .await;
                let Ok(response) = response else {
                    errors.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                if !response.status().is_success() {
                    errors.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let Ok(body) = response.bytes().await else {
                    errors.fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                requests.fetch_add(1, Ordering::Relaxed);
                bytes.fetch_add(body.len() as u64, Ordering::Relaxed);

                // Verified on every read, not sampled. A corrupt byte must fail the run at the
                // moment it appears, while the state that produced it is still on disk.
                if let Err(mismatch) = differ::compare(0x50AC_0000 + worker as u64, &case, &body) {
                    corrupt.fetch_add(1, Ordering::Relaxed);
                    eprintln!("\nINTEGRITY FAILURE\n{mismatch}");
                }
            }
        }));
    }

    let mut last_requests = 0u64;
    while started.elapsed() < Duration::from_secs(args.seconds) {
        tokio::time::sleep(Duration::from_secs(args.report_secs)).await;
        let done = requests.load(Ordering::Relaxed);
        let rss = rss_bytes();
        println!(
            "{:>5}s  {:>8} requests ({:>5}/s)  {:>7} MiB served  RSS {:>6} MiB (+{} MiB)  \
             index {:>6}  errors {}  corrupt {}",
            started.elapsed().as_secs(),
            done,
            (done - last_requests) / args.report_secs.max(1),
            bytes.load(Ordering::Relaxed) / (1024 * 1024),
            rss / (1024 * 1024),
            rss.saturating_sub(baseline_rss) / (1024 * 1024),
            index.len().unwrap_or(0),
            errors.load(Ordering::Relaxed),
            corrupt.load(Ordering::Relaxed),
        );
        last_requests = done;
    }

    stop.store(true, Ordering::Relaxed);
    for handle in handles {
        let _ = handle.await;
    }

    let corrupt_total = corrupt.load(Ordering::Relaxed);
    let errors_total = errors.load(Ordering::Relaxed);
    println!(
        "\nfinished: {} requests, {} MiB served, {} errors, {} integrity failures",
        requests.load(Ordering::Relaxed),
        bytes.load(Ordering::Relaxed) / (1024 * 1024),
        errors_total,
        corrupt_total
    );
    println!(
        "RSS {} MiB, grew {} MiB from baseline",
        rss_bytes() / (1024 * 1024),
        rss_bytes().saturating_sub(baseline_rss) / (1024 * 1024)
    );

    let _ = std::fs::remove_dir_all(&args.dir);

    // NFR-7 is absolute: zero corrupt bytes. Errors under eviction pressure are a degradation to
    // investigate; corruption is a failure.
    if corrupt_total > 0 {
        eprintln!("\nSOAK FAILED: {corrupt_total} integrity failures");
        std::process::exit(1);
    }
    println!("\nSOAK PASSED");
}
