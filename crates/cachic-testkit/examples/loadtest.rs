//! Drives load against a *running* proxy and reports throughput and TTFB.
//!
//! The existing harnesses link the proxy into the test process, which is the right shape for
//! correctness work and the wrong shape for two things: comparing cachic against another
//! implementation, and measuring a container from outside it. This drives anything that speaks
//! HTTP, so cachic and `lancachenet/monolithic` can be put under identical load and compared.
//!
//! It is a measurement tool, so it avoids being the bottleneck: one connection per client, kept
//! alive, and the body is drained without being retained.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use cachic_testkit::loadgen::Collector;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "loadtest", about = "drive load against a running proxy")]
struct Args {
    /// Proxy base URL, e.g. http://127.0.0.1:8080
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    target: String,
    /// Host header, which is how the proxy picks a service.
    #[arg(long, default_value = "lancache.steamcontent.com")]
    host: String,
    #[arg(long, default_value_t = 32)]
    clients: usize,
    #[arg(long, default_value_t = 60)]
    seconds: u64,
    /// Distinct objects in the working set.
    #[arg(long, default_value_t = 24)]
    objects: usize,
    #[arg(long, default_value_t = 256)]
    object_mib: u64,
    /// Range size per request, in MiB. One of these is chosen at random per request.
    #[arg(long, value_delimiter = ',', default_values_t = [1u64, 2, 4])]
    range_mib: Vec<u64>,
    /// Report progress this often.
    #[arg(long, default_value_t = 20)]
    report_secs: u64,
}

/// xorshift, so each client has a deterministic independent stream without a rand dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let object_size = args.object_mib * 1024 * 1024;
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();

    let mut workers = Vec::new();
    for id in 0..args.clients {
        let args_target = args.target.clone();
        let args_host = args.host.clone();
        let ranges = args.range_mib.clone();
        let objects = args.objects;
        let stop = stop.clone();
        workers.push(tokio::spawn(async move {
            // One client per worker so connections are not shared, matching a real client mix.
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("client");
            let mut rng = Rng(0x9e3779b97f4a7c15 ^ (id as u64 + 1).wrapping_mul(0x100000001b3));
            let mut collector = Collector::new();
            let mut errors = 0u64;

            while !stop.load(Ordering::Relaxed) {
                let object = (rng.next() as usize) % objects;
                let span = ranges[(rng.next() as usize) % ranges.len()] * 1024 * 1024;
                let start = (rng.next() % (object_size - span)) / 4096 * 4096;
                let end = start + span - 1;

                let began = Instant::now();
                let sent = client
                    .get(format!("{args_target}/o/obj{object}/{object_size}"))
                    .header("Host", &args_host)
                    .header("Range", format!("bytes={start}-{end}"))
                    .send()
                    .await;
                let Ok(response) = sent else {
                    errors += 1;
                    continue;
                };
                if !response.status().is_success() {
                    errors += 1;
                    continue;
                }
                let ttfb = began.elapsed();
                match response.bytes().await {
                    Ok(body) => collector.record(body.len() as u64, ttfb),
                    Err(_) => errors += 1,
                }
            }
            (collector, errors)
        }));
    }

    let mut ticker = tokio::time::interval(Duration::from_secs(args.report_secs));
    ticker.tick().await;
    while started.elapsed() < Duration::from_secs(args.seconds) {
        ticker.tick().await;
        println!("  {:>4}s elapsed", started.elapsed().as_secs());
    }
    stop.store(true, Ordering::Relaxed);

    // Merge the per-worker samples. Each worker keeps its own collector so the hot path never
    // touches a shared lock, which would make the generator measure itself.
    let mut total = Collector::new();
    let mut errors = 0u64;
    for worker in workers {
        let (collector, worker_errors) = worker.await?;
        errors += worker_errors;
        let report = collector.finish(Duration::ZERO);
        let per_request = if report.requests > 0 {
            report.bytes / report.requests as u64
        } else {
            0
        };
        let mut remaining = report.bytes;
        for (i, ttfb) in report.ttfb.iter().enumerate() {
            // Give the last sample the rounding remainder so the byte total is exact.
            let bytes = if i + 1 == report.ttfb.len() {
                remaining
            } else {
                per_request
            };
            remaining = remaining.saturating_sub(bytes);
            total.record(bytes, *ttfb);
        }
    }
    let report = total.finish(started.elapsed());
    println!("{}", report.summary());
    println!("errors {errors}");
    Ok(())
}
