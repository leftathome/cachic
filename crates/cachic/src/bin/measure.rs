//! M0 measurement harness (TASK-04).
//!
//! Produces the numbers the ADRs cite. Emits CSV on stdout so results can be committed under
//! `docs/benchmarks/` alongside the exact command that produced them.
//!
//! Every number is only valid for the hardware it was taken on. The M0 exit criterion is
//! specified on an amd64 NUC with NVMe; runs anywhere else are a provisional signal and the
//! report must say so.

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use cachic::spike::{
    proxy::{SpikeConfig, SpikeProxy},
    slice::{object_id, SliceHeader, SliceKey, SliceValue},
    store::{SpikeStore, StoreConfig},
};
use cachic_testkit::mockcdn::{Config as CdnConfig, MockCdn};
use clap::{Parser, Subcommand};

const MIB: usize = 1024 * 1024;

#[derive(Parser, Debug)]
#[command(name = "measure", about = "M0 measurements for cachic")]
struct Args {
    /// Directory for scratch cache data. Must be on native storage: measuring a DrvFs path
    /// under WSL2 measures the Windows filesystem bridge, not the cache.
    #[arg(long, default_value = "/tmp/cachic-measure")]
    dir: PathBuf,

    /// Use O_DIRECT for the disk tier. foyer's default is true; on some filesystems (WSL2's
    /// virtual ext4 among them) that silently loses entries across a reopen.
    #[arg(long, default_value_t = false)]
    direct: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Slice encode/decode and checksum throughput.
    Codec {
        #[arg(long, default_value_t = 1)]
        slice_mib: usize,
        #[arg(long, default_value_t = 512)]
        iterations: usize,
    },
    /// foyer write and read throughput, memory tier and disk tier.
    Store {
        #[arg(long, default_value_t = 1)]
        slice_mib: usize,
        #[arg(long, default_value_t = 2048)]
        entries: usize,
        /// Memory tier size in MiB. Set small to force disk-tier reads.
        #[arg(long, default_value_t = 2048)]
        mem_mib: usize,
        /// Disk tier size in MiB. Defaults to twice the data written.
        #[arg(long)]
        disk_mib: Option<usize>,
        /// Disk block size in MiB.
        #[arg(long, default_value_t = 64)]
        block_mib: usize,
    },
    /// Resident memory per indexed entry. Uses tiny payloads so the index cost is not swamped
    /// by slice data; the per-entry overhead is what sizes CACHE_MEM_SIZE.
    IndexMemory {
        #[arg(long, default_value = "100000,1000000")]
        entries: String,
    },
    /// End-to-end hit throughput and time-to-first-byte through the spike proxy.
    Proxy {
        #[arg(long, default_value_t = 8)]
        clients: usize,
        #[arg(long, default_value_t = 256)]
        object_mib: usize,
        #[arg(long, default_value_t = 1)]
        slice_mib: usize,
        /// Memory tier in MiB. Larger than the object measures RAM-tier hits; much smaller
        /// measures disk-tier hits.
        #[arg(long, default_value_t = 512)]
        mem_mib: usize,
        #[arg(long, default_value_t = 3)]
        rounds: usize,
    },
    /// Time to reopen a populated store and read the first slice.
    Recovery {
        #[arg(long, default_value_t = 1)]
        slice_mib: usize,
        #[arg(long, default_value_t = 4096)]
        entries: usize,
    },
}

fn row(scenario: &str, metric: &str, value: impl std::fmt::Display, unit: &str, notes: &str) {
    println!("{scenario},{metric},{value},{unit},{notes}");
}

fn header() {
    println!("scenario,metric,value,unit,notes");
}

/// Resident set size in bytes, from /proc. Linux only, which is our tier-1 platform.
fn rss_bytes() -> u64 {
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            return kb * 1024;
        }
    }
    0
}

fn header_for(total_len: u64, slice_size: u32) -> SliceHeader {
    SliceHeader {
        slice_size,
        total_len,
        generation: 0,
        etag: Some("\"measure\"".into()),
        last_modified: None,
        content_type: Some("application/octet-stream".into()),
    }
}

fn gbps(bytes: u64, elapsed: Duration) -> f64 {
    (bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1e9
}

fn mibs(bytes: u64, elapsed: Duration) -> f64 {
    (bytes as f64 / MIB as f64) / elapsed.as_secs_f64()
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx]
}

async fn scratch(dir: &Path, tag: &str) -> PathBuf {
    let path = dir.join(tag);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn measure_codec(slice_mib: usize, iterations: usize) {
    use foyer::Code;

    let payload = Bytes::from(vec![0x5Au8; slice_mib * MIB]);
    let value = SliceValue::new(header_for(1 << 30, (slice_mib * MIB) as u32), payload);
    let mut buf = Vec::with_capacity(value.estimated_size());

    let start = Instant::now();
    for _ in 0..iterations {
        buf.clear();
        value.encode(&mut buf).unwrap();
    }
    let encode = start.elapsed();
    let bytes = (slice_mib * MIB * iterations) as u64;
    row(
        "codec",
        "encode_throughput",
        format!("{:.2}", mibs(bytes, encode)),
        "MiB/s",
        "includes xxh3 checksum",
    );
    row(
        "codec",
        "encode_throughput",
        format!("{:.2}", gbps(bytes, encode)),
        "Gbps",
        "",
    );

    let start = Instant::now();
    for _ in 0..iterations {
        let decoded = SliceValue::decode(&mut buf.as_slice()).unwrap();
        std::hint::black_box(&decoded);
    }
    let decode = start.elapsed();
    row(
        "codec",
        "decode_throughput",
        format!("{:.2}", mibs(bytes, decode)),
        "MiB/s",
        "includes checksum verification and a copy",
    );
    row(
        "codec",
        "decode_throughput",
        format!("{:.2}", gbps(bytes, decode)),
        "Gbps",
        "",
    );
}

/// Build a payload unique to `index`.
///
/// This matters more than it looks: `Bytes::clone` is a refcount bump, so reusing one buffer
/// across every entry makes the store hold a single allocation and turns reads into no-ops. The
/// first measurement run of this harness reported 20 Tbps for exactly that reason.
fn distinct_payload(index: usize, len: usize) -> Bytes {
    let mut v = vec![0u8; len];
    let tag = (index as u64).to_le_bytes();
    for (i, b) in v.iter_mut().enumerate() {
        *b = tag[i % 8] ^ (i as u8);
    }
    Bytes::from(v)
}

/// Consume a payload so the read cannot be optimised away or reduced to a refcount bump.
fn consume(payload: &Bytes) -> u64 {
    xxhash_rust::xxh3::xxh3_64(payload)
}

async fn measure_store(
    direct: bool,
    dir: &Path,
    slice_mib: usize,
    entries: usize,
    mem_mib: usize,
    disk_mib: Option<usize>,
    block_mib: usize,
) {
    let path = scratch(dir, "store").await;
    let slice_size = (slice_mib * MIB) as u32;
    let len = slice_mib * MIB;
    let total = (entries * len) as u64;
    let bytes = (entries * len) as u64;

    let store = SpikeStore::open(
        &path,
        &StoreConfig {
            memory_bytes: mem_mib * MIB,
            disk_bytes: disk_mib
                .map(|m| m * MIB)
                .unwrap_or((entries * len * 2).max(256 * MIB)),
            block_bytes: block_mib * MIB,
            direct_io: direct,
        },
    )
    .await
    .unwrap();

    let object = object_id("/measure/store");
    let hdr = header_for(total, slice_size);

    // Cost of generating the payloads, so it can be subtracted from the write figure rather
    // than silently inflating it.
    let start = Instant::now();
    for i in 0..entries {
        std::hint::black_box(distinct_payload(i, len));
    }
    let generation = start.elapsed();
    row(
        "store",
        "config",
        format!(
            "mem={mem_mib}MiB disk={}MiB block={block_mib}MiB data={}MiB direct={direct}",
            disk_mib.unwrap_or((entries * len * 2).max(256 * MIB) / MIB),
            entries * len / MIB
        ),
        "",
        "",
    );
    row(
        "store",
        "payload_generation",
        format!("{:.2}", mibs(bytes, generation)),
        "MiB/s",
        "allocation and fill only, no store involved",
    );

    let rss_before = rss_bytes();
    let start = Instant::now();
    for i in 0..entries {
        let value = SliceValue::new(hdr.clone(), distinct_payload(i, len));
        store
            .get_or_fetch(SliceKey::new(object, 0, i as u32), move || async move {
                Ok(value)
            })
            .await
            .unwrap();
    }
    let write = start.elapsed();
    let write_net = write.saturating_sub(generation);
    row("store", "insert_rate", format!("{:.2}", mibs(bytes, write)), "MiB/s", &format!("{entries} distinct entries of {slice_mib} MiB, mem tier {mem_mib} MiB; includes payload generation"));
    row(
        "store",
        "insert_rate_net",
        format!("{:.2}", mibs(bytes, write_net)),
        "MiB/s",
        "payload generation subtracted",
    );
    row(
        "store",
        "insert_rate_net",
        format!("{:.2}", gbps(bytes, write_net)),
        "Gbps",
        "",
    );
    row(
        "store",
        "rss_after_insert",
        rss_bytes(),
        "bytes",
        &format!(
            "delta {} over {bytes} bytes inserted",
            rss_bytes().saturating_sub(rss_before)
        ),
    );

    // Inserts are queued to disk asynchronously, so the insert rate above is not a disk write
    // rate. Closing flushes; the difference is what the disk tier actually costs.
    let start = Instant::now();
    store.close().await.unwrap();
    let flush = start.elapsed();
    row(
        "store",
        "flush_time",
        format!("{:.3}", flush.as_secs_f64()),
        "s",
        "close() after the inserts above",
    );
    row(
        "store",
        "insert_plus_flush",
        format!("{:.2}", mibs(bytes, write_net + flush)),
        "MiB/s",
        "durable write throughput",
    );
    row(
        "store",
        "insert_plus_flush",
        format!("{:.2}", gbps(bytes, write_net + flush)),
        "Gbps",
        "",
    );

    // Reopen so reads are served from whatever survived, not from a warm in-process memory tier.
    let store = SpikeStore::open(
        &path,
        &StoreConfig {
            memory_bytes: mem_mib * MIB,
            disk_bytes: (entries * len * 2).max(256 * MIB),
            block_bytes: 64 * MIB,
            direct_io: direct,
        },
    )
    .await
    .unwrap();

    // Two passes. The first reads a cold process: whatever the memory tier holds is empty, so
    // this is the disk tier plus the cost of populating memory. Only the second pass measures
    // memory-tier hits, and then only if the tier is large enough to have retained the data.
    let mut pass_results = Vec::new();
    for pass in 0..2 {
        let start = Instant::now();
        let mut checksum = 0u64;
        let mut misses = 0usize;
        for i in 0..entries {
            let key = SliceKey::new(object, 0, i as u32);
            match store
                .get_or_fetch(key, || async { anyhow::bail!("miss") })
                .await
            {
                Ok(v) => checksum ^= consume(&v.payload),
                Err(_) => misses += 1,
            }
        }
        let elapsed = start.elapsed();
        std::hint::black_box(checksum);
        pass_results.push((pass, elapsed, misses));
    }

    let fits_in_memory = mem_mib * MIB >= entries * len;
    for (pass, elapsed, misses) in pass_results {
        let read_bytes = ((entries - misses) * len) as u64;
        let label = match (pass, fits_in_memory) {
            (0, _) => "cold process: disk tier plus memory-tier population",
            (_, true) => "warm: memory tier",
            (_, false) => "warm: disk tier, memory tier too small to retain",
        };
        row(
            "store",
            &format!("read_throughput_pass{pass}"),
            format!("{:.2}", mibs(read_bytes, elapsed)),
            "MiB/s",
            &format!("{label}; {misses} misses; includes xxh3 over every byte"),
        );
        row(
            "store",
            &format!("read_throughput_pass{pass}"),
            format!("{:.2}", gbps(read_bytes, elapsed)),
            "Gbps",
            "",
        );
        row(
            "store",
            &format!("read_misses_pass{pass}"),
            misses,
            "count",
            &format!("of {entries}"),
        );
    }

    // Closing a store that has served failed fetches has been observed to hang; time it out so
    // the harness always produces a report. See docs/adr/0003 and the M0 findings.
    let start = Instant::now();
    let closed = tokio::time::timeout(Duration::from_secs(20), store.close()).await;
    match closed {
        Ok(Ok(())) => row(
            "store",
            "close_time",
            format!("{:.3}", start.elapsed().as_secs_f64()),
            "s",
            "second close, after reads",
        ),
        Ok(Err(e)) => row(
            "store",
            "close_time",
            "error",
            "s",
            &format!("{e}").replace(',', ";"),
        ),
        Err(_) => row(
            "store",
            "close_time",
            "timeout",
            "s",
            "close() did not return within 20s after reads that included failed fetches",
        ),
    }
    let _ = std::fs::remove_dir_all(&path);
}

async fn measure_index_memory(direct: bool, dir: &Path, entries_spec: &str) {
    for spec in entries_spec.split(',') {
        let entries: usize = match spec.trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path = scratch(dir, &format!("index-{entries}")).await;
        // Tiny payloads: we are measuring per-entry index overhead, not slice data.
        let payload = Bytes::from_static(b"x");
        let store = SpikeStore::open(
            &path,
            &StoreConfig {
                // Memory tier large enough to hold every entry, so nothing is evicted and the
                // measurement reflects a fully indexed cache.
                memory_bytes: (entries * 256).max(64 * MIB),
                disk_bytes: 1024 * MIB,
                block_bytes: 16 * MIB,
                direct_io: direct,
            },
        )
        .await
        .unwrap();

        let object = object_id("/measure/index");
        let hdr = header_for(entries as u64, 1);
        let baseline = rss_bytes();
        let start = Instant::now();
        for i in 0..entries {
            let value = SliceValue::new(hdr.clone(), payload.clone());
            store
                .get_or_fetch(SliceKey::new(object, 0, i as u32), move || async move {
                    Ok(value)
                })
                .await
                .unwrap();
        }
        let elapsed = start.elapsed();
        let after = rss_bytes();
        let delta = after.saturating_sub(baseline);
        row(
            "index_memory",
            "rss_per_entry",
            format!("{:.1}", delta as f64 / entries as f64),
            "bytes",
            &format!(
                "{entries} entries, RSS {baseline} -> {after}, insert took {:.2}s",
                elapsed.as_secs_f64()
            ),
        );
        row(
            "index_memory",
            "insert_rate",
            format!("{:.0}", entries as f64 / elapsed.as_secs_f64()),
            "entries/s",
            "",
        );
        store.close().await.unwrap();
        let _ = std::fs::remove_dir_all(&path);
    }
}

async fn measure_proxy(
    direct: bool,
    dir: &Path,
    clients: usize,
    object_mib: usize,
    slice_mib: usize,
    mem_mib: usize,
    rounds: usize,
) {
    let path = scratch(dir, "proxy").await;
    let origin = MockCdn::start(CdnConfig::default()).await.unwrap();
    let size = (object_mib * MIB) as u64;

    let mut config = SpikeConfig::new(origin.base_url(), &path);
    config.slice_size = (slice_mib * MIB) as u32;
    config.readahead = 8;
    config.store = StoreConfig {
        memory_bytes: mem_mib * MIB,
        disk_bytes: (object_mib * MIB * 4).max(512 * MIB),
        block_bytes: 64 * MIB,
        direct_io: direct,
    };
    let proxy = Arc::new(SpikeProxy::start(config).await.unwrap());
    let url = format!(
        "{}{}",
        proxy.base_url(),
        MockCdn::object_path("bench", size)
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .unwrap();

    // Cold fill, single client: this is the miss path, measured against the origin's own rate.
    let start = Instant::now();
    let body = client
        .get(&url)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let cold = start.elapsed();
    assert_eq!(body.len() as u64, size);
    row(
        "proxy",
        "cold_fill_throughput",
        format!("{:.2}", gbps(size, cold)),
        "Gbps",
        "single client, mock origin on loopback",
    );

    let direct_url = origin.object_url("bench", size);
    let start = Instant::now();
    let direct = client
        .get(&direct_url)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let direct_elapsed = start.elapsed();
    assert_eq!(direct.len() as u64, size);
    row(
        "proxy",
        "origin_direct_throughput",
        format!("{:.2}", gbps(size, direct_elapsed)),
        "Gbps",
        "same object straight from the origin",
    );
    row(
        "proxy",
        "cold_fill_overhead",
        format!(
            "{:.1}",
            (cold.as_secs_f64() / direct_elapsed.as_secs_f64() - 1.0) * 100.0
        ),
        "percent",
        "proxy cold fill versus direct; NFR-3 wants fill >= 95% of direct",
    );

    // Warm reads, N concurrent clients.
    for round in 0..rounds {
        let ttfb = Arc::new(std::sync::Mutex::new(Vec::new()));
        let bytes_read = Arc::new(AtomicU64::new(0));
        let start = Instant::now();
        let mut handles = Vec::new();
        for _ in 0..clients {
            let client = client.clone();
            let url = url.clone();
            let ttfb = ttfb.clone();
            let bytes_read = bytes_read.clone();
            handles.push(tokio::spawn(async move {
                let t0 = Instant::now();
                let response = client.get(url).send().await.unwrap();
                assert_eq!(response.status(), 200);
                let mut stream = response.bytes_stream();
                let mut first = true;
                let mut n = 0u64;
                use futures_util::StreamExt;
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.unwrap();
                    if first {
                        ttfb.lock().unwrap().push(t0.elapsed());
                        first = false;
                    }
                    n += chunk.len() as u64;
                }
                bytes_read.fetch_add(n, Ordering::Relaxed);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let elapsed = start.elapsed();
        let total = bytes_read.load(Ordering::Relaxed);
        assert_eq!(total, size * clients as u64);

        let mut samples = ttfb.lock().unwrap().clone();
        samples.sort();
        let tier = if mem_mib * MIB >= object_mib * MIB {
            "memory tier"
        } else {
            "disk tier"
        };
        row(
            "proxy",
            "hit_throughput",
            format!("{:.2}", gbps(total, elapsed)),
            "Gbps",
            &format!("round {round}, {clients} clients, {tier}"),
        );
        row(
            "proxy",
            "ttfb_p50",
            format!("{:.2}", percentile(&samples, 0.50).as_secs_f64() * 1000.0),
            "ms",
            &format!("round {round}, {tier}"),
        );
        row(
            "proxy",
            "ttfb_p99",
            format!("{:.2}", percentile(&samples, 0.99).as_secs_f64() * 1000.0),
            "ms",
            &format!("round {round}, {tier}"),
        );
        row(
            "proxy",
            "rss",
            rss_bytes(),
            "bytes",
            &format!("round {round}; mem tier {mem_mib} MiB"),
        );
    }

    row(
        "proxy",
        "upstream_requests_total",
        origin.stats().requests(),
        "count",
        "cold fill plus one direct read",
    );
    row(
        "proxy",
        "upstream_bytes_total",
        origin.stats().bytes_served(),
        "bytes",
        &format!("object is {size} bytes"),
    );

    proxy.close().await.unwrap();
    let _ = std::fs::remove_dir_all(&path);
}

async fn measure_recovery(direct: bool, dir: &Path, slice_mib: usize, entries: usize) {
    let path = scratch(dir, "recovery").await;
    let slice_size = (slice_mib * MIB) as u32;
    let payload = Bytes::from(vec![0x37u8; slice_mib * MIB]);
    let total = (entries * slice_mib * MIB) as u64;
    let config = StoreConfig {
        memory_bytes: 128 * MIB,
        disk_bytes: (entries * slice_mib * MIB * 2).max(512 * MIB),
        block_bytes: 64 * MIB,
        direct_io: direct,
    };
    let object = object_id("/measure/recovery");
    let hdr = header_for(total, slice_size);

    {
        let store = SpikeStore::open(&path, &config).await.unwrap();
        for i in 0..entries {
            let value = SliceValue::new(hdr.clone(), payload.clone());
            store
                .get_or_fetch(SliceKey::new(object, 0, i as u32), move || async move {
                    Ok(value)
                })
                .await
                .unwrap();
        }
        store.close().await.unwrap();
    }

    let cached_bytes = (entries * slice_mib * MIB) as u64;
    let start = Instant::now();
    let store = SpikeStore::open(&path, &config).await.unwrap();
    let open = start.elapsed();
    row(
        "recovery",
        "store_open_time",
        format!("{:.3}", open.as_secs_f64()),
        "s",
        &format!("{cached_bytes} bytes cached across {entries} slices"),
    );

    let start = Instant::now();
    let value = store
        .get_or_fetch(SliceKey::new(object, 0, 0), || async {
            anyhow::bail!("slice lost across restart")
        })
        .await;
    let first_hit = start.elapsed();
    match value {
        Ok(v) => {
            assert_eq!(v.payload.len(), slice_mib * MIB);
            row(
                "recovery",
                "time_to_first_hit",
                format!("{:.3}", first_hit.as_secs_f64() * 1000.0),
                "ms",
                "slice 0 read after reopen",
            );
            row("recovery", "survived_restart", "true", "bool", "");
        }
        Err(e) => {
            row(
                "recovery",
                "survived_restart",
                "false",
                "bool",
                &format!("{e}").replace(',', ";"),
            );
        }
    }

    // How many of the originally written slices are still readable after a restart?
    let mut recovered = 0usize;
    for i in 0..entries {
        if store.contains(&SliceKey::new(object, 0, i as u32)) {
            recovered += 1;
        }
    }
    row(
        "recovery",
        "slices_recovered",
        recovered,
        "count",
        &format!("of {entries} written"),
    );
    row(
        "recovery",
        "recovery_ratio",
        format!("{:.3}", recovered as f64 / entries as f64),
        "ratio",
        "",
    );

    store.close().await.unwrap();
    let _ = std::fs::remove_dir_all(&path);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.dir)?;
    header();
    match args.command {
        Command::Codec {
            slice_mib,
            iterations,
        } => measure_codec(slice_mib, iterations),
        Command::Store {
            slice_mib,
            entries,
            mem_mib,
            disk_mib,
            block_mib,
        } => {
            measure_store(
                args.direct,
                &args.dir,
                slice_mib,
                entries,
                mem_mib,
                disk_mib,
                block_mib,
            )
            .await
        }
        Command::IndexMemory { entries } => {
            measure_index_memory(args.direct, &args.dir, &entries).await
        }
        Command::Proxy {
            clients,
            object_mib,
            slice_mib,
            mem_mib,
            rounds,
        } => {
            measure_proxy(
                args.direct,
                &args.dir,
                clients,
                object_mib,
                slice_mib,
                mem_mib,
                rounds,
            )
            .await
        }
        Command::Recovery { slice_mib, entries } => {
            measure_recovery(args.direct, &args.dir, slice_mib, entries).await
        }
    }
    Ok(())
}
