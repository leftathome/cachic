//! foyer ingest-rate probe.
//!
//! foyer silently discards a disk write when its flushers cannot keep up, incrementing
//! `storage_queue_channel_overflow` and returning nothing to the caller. For a cache that is a
//! defensible design - a dropped write is a future miss - but it means cachic has a maximum
//! sustainable fill rate, above which the cache quietly stops caching while still serving clients
//! at full speed.
//!
//! That rate matters directly: the PRD's primary persona is a homelab on 10 GbE, and modern fibre
//! reaches 1-10 Gbit/s. This probe measures where the ceiling actually is and which knobs move it.
//!
//! Writes are paced to a target rate rather than issued as fast as possible, because an unpaced
//! writer measures a rate no real workload produces. Entries are counted while the process is
//! still live *and* after a reopen, because a large memory tier hides dropped disk writes behind
//! RAM hits until a restart exposes them.

use std::time::{Duration, Instant};

use foyer::{
    BlockEngineConfig, DeviceBuilder, FsDeviceBuilder, HybridCache, HybridCacheBuilder,
    HybridCachePolicy, PsyncIoEngineConfig,
};

const MIB: usize = 1024 * 1024;

/// Fill rates worth caring about, in MiB/s, with the WAN tier that produces them.
const FIBRE_TIERS: &[(&str, f64)] = &[
    ("1 Gbit", 119.2),
    ("2.5 Gbit", 298.0),
    ("5 Gbit", 596.0),
    ("10 Gbit", 1192.1),
];

#[derive(Clone, Copy)]
struct Trial {
    label: &'static str,
    entries: usize,
    entry_kib: usize,
    mem_mib: usize,
    disk_mib: usize,
    block_mib: usize,
    /// foyer default: 1.
    flushers: usize,
    /// foyer default: 16 MiB, shared across flushers.
    buffer_pool_mib: usize,
    /// foyer default: 16 MiB.
    submit_queue_mib: usize,
    /// Target write rate in MiB/s. `None` writes as fast as possible.
    target_mibs: Option<f64>,
}

async fn open(t: &Trial, dir: &std::path::Path) -> HybridCache<u64, Vec<u8>> {
    let device = FsDeviceBuilder::new(dir)
        .with_capacity(t.disk_mib * MIB)
        .with_direct(false)
        .build()
        .unwrap();
    HybridCacheBuilder::new()
        .with_name("probe")
        .with_policy(HybridCachePolicy::WriteOnInsertion)
        .with_flush_on_close(true)
        .memory(t.mem_mib * MIB)
        .with_weighter(|_k: &u64, v: &Vec<u8>| 8 + v.len())
        .storage()
        .with_io_engine_config(PsyncIoEngineConfig::new())
        .with_engine_config(
            BlockEngineConfig::new(device)
                .with_block_size(t.block_mib * MIB)
                .with_flushers(t.flushers)
                .with_buffer_pool_size(t.buffer_pool_mib * MIB)
                .with_submit_queue_size_threshold(t.submit_queue_mib * MIB),
        )
        .build()
        .await
        .unwrap()
}

async fn count(cache: &HybridCache<u64, Vec<u8>>, entries: usize) -> usize {
    let mut n = 0;
    for i in 0..entries {
        if cache.get(&(i as u64)).await.unwrap().is_some() {
            n += 1;
        }
    }
    n
}

/// Result of one trial.
struct Outcome {
    live: usize,
    recovered: usize,
    achieved_mibs: f64,
}

async fn trial(t: Trial) -> Outcome {
    let dir = std::path::PathBuf::from(format!("/root/.cache/foyerprobe-{}", t.label));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let live;
    let achieved_mibs;
    {
        let cache = open(&t, &dir).await;
        let start = Instant::now();
        for i in 0..t.entries {
            // Pace against the wall clock rather than sleeping a fixed amount per entry, so the
            // achieved rate tracks the target even when an individual insert stalls.
            if let Some(target) = t.target_mibs {
                let due = Duration::from_secs_f64((i * t.entry_kib) as f64 / 1024.0 / target);
                let elapsed = start.elapsed();
                if due > elapsed {
                    tokio::time::sleep(due - elapsed).await;
                }
            }
            cache.insert(i as u64, vec![(i % 251) as u8; t.entry_kib * 1024]);
        }
        let elapsed = start.elapsed();
        achieved_mibs = (t.entries * t.entry_kib) as f64 / 1024.0 / elapsed.as_secs_f64();
        live = count(&cache, t.entries).await;
        cache.close().await.unwrap();
    }

    let cache = open(&t, &dir).await;
    let recovered = count(&cache, t.entries).await;
    let _ = cache.close().await;
    let _ = std::fs::remove_dir_all(&dir);

    Outcome {
        live,
        recovered,
        achieved_mibs,
    }
}

fn report(what: &str, t: &Trial, o: &Outcome) {
    let pct = |n: usize| n as f64 / t.entries as f64 * 100.0;
    println!(
        "{what:<34} {:>7.0} MiB/s achieved  live {:>5.1}%  reopened {:>5.1}%   \
         [flushers {}, pool {} MiB, queue {} MiB]",
        o.achieved_mibs,
        pct(o.live),
        pct(o.recovered),
        t.flushers,
        t.buffer_pool_mib,
        t.submit_queue_mib,
    );
}

#[tokio::main]
async fn main() {
    // 512 MiB of data into a 4 GiB disk tier: nothing here is an eviction for capacity. The
    // memory tier is deliberately small so RAM hits cannot mask a dropped disk write.
    let base = Trial {
        label: "probe",
        entries: 512,
        entry_kib: 1024,
        mem_mib: 64,
        disk_mib: 4096,
        block_mib: 16,
        flushers: 1,
        buffer_pool_mib: 16,
        submit_queue_mib: 16,
        target_mibs: None,
    };

    println!("== foyer defaults (1 flusher, 16 MiB pool) against real fibre tiers ==");
    for (tier, mibs) in FIBRE_TIERS {
        let t = Trial {
            target_mibs: Some(*mibs),
            ..base
        };
        let o = trial(t).await;
        report(&format!("{tier} ({mibs:.0} MiB/s target)"), &t, &o);
    }

    println!();
    println!("== can the ceiling be raised? 10 Gbit target, tuned ==");
    for (flushers, buffer_pool_mib, submit_queue_mib) in [(2, 64, 64), (4, 128, 128), (8, 256, 256)]
    {
        let t = Trial {
            flushers,
            buffer_pool_mib,
            submit_queue_mib,
            target_mibs: Some(1192.1),
            ..base
        };
        let o = trial(t).await;
        report("10 Gbit tuned", &t, &o);
    }
}
