//! Minimal foyer behaviour probe.
//!
//! Answers one question: when we write N entries, how many does foyer actually keep - while the
//! process is still running, and again after a clean close and reopen?
//!
//! It talks to foyer directly, with plain `u64` keys and `Vec<u8>` values, so nothing here can be
//! blamed on cachic's slice codec or store wrapper.
//!
//! Reading back *before* the close is the important part: it separates "entries were dropped on
//! the way to disk" from "entries were written and then not recovered". Those look identical if
//! you only measure after a restart, and they have completely different fixes.

use std::time::{Duration, Instant};

use foyer::{
    BlockEngineConfig, DeviceBuilder, FsDeviceBuilder, HybridCache, HybridCacheBuilder,
    HybridCachePolicy, PsyncIoEngineConfig,
};

const MIB: usize = 1024 * 1024;

/// One trial: populate, count live, close, reopen, count again.
#[derive(Clone, Copy)]
struct Trial {
    label: &'static str,
    /// Populate with `insert` rather than `get_or_fetch`.
    use_insert: bool,
    policy: HybridCachePolicy,
    entries: usize,
    entry_kib: usize,
    mem_mib: usize,
    disk_mib: usize,
    block_mib: usize,
    /// Bytes allowed in foyer's disk write queue before further entries are ignored.
    /// foyer's default is 16 MiB.
    submit_queue_mib: usize,
    /// Pause between inserts, so the writer can be made slower than the flusher.
    insert_delay_ms: u64,
}

async fn open(t: &Trial, dir: &std::path::Path) -> HybridCache<u64, Vec<u8>> {
    let device = FsDeviceBuilder::new(dir)
        .with_capacity(t.disk_mib * MIB)
        .with_direct(false)
        .build()
        .unwrap();
    HybridCacheBuilder::new()
        .with_name("probe")
        .with_policy(t.policy)
        .with_flush_on_close(true)
        .memory(t.mem_mib * MIB)
        .with_weighter(|_k: &u64, v: &Vec<u8>| 8 + v.len())
        .storage()
        .with_io_engine_config(PsyncIoEngineConfig::new())
        .with_engine_config(
            BlockEngineConfig::new(device)
                .with_block_size(t.block_mib * MIB)
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

async fn trial(t: Trial) {
    let dir = std::path::PathBuf::from(format!("/root/.cache/foyerprobe-{}", t.label));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let live;
    let write_elapsed;
    {
        let cache = open(&t, &dir).await;
        let start = Instant::now();
        for i in 0..t.entries {
            if t.insert_delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(t.insert_delay_ms)).await;
            }
            let payload = vec![(i % 251) as u8; t.entry_kib * 1024];
            if t.use_insert {
                cache.insert(i as u64, payload);
            } else {
                cache
                    .get_or_fetch(
                        &(i as u64),
                        || async move { Ok::<_, anyhow::Error>(payload) },
                    )
                    .await
                    .unwrap();
            }
        }
        write_elapsed = start.elapsed();
        live = count(&cache, t.entries).await;
        cache.close().await.unwrap();
    }

    let cache = open(&t, &dir).await;
    let recovered = count(&cache, t.entries).await;
    let _ = cache.close().await;
    let _ = std::fs::remove_dir_all(&dir);

    let data_mib = t.entries * t.entry_kib / 1024;
    let rate = data_mib as f64 / write_elapsed.as_secs_f64();
    println!(
        "{:<10} live {live:>5}/{:<5} ({:>5.1}%)  reopened {recovered:>5}/{:<5} ({:>5.1}%)  \
         [{data_mib} MiB @ {rate:.0} MiB/s, mem {} MiB, disk {} MiB, block {} MiB, queue {} MiB, delay {} ms]",
        t.label,
        t.entries,
        live as f64 / t.entries as f64 * 100.0,
        t.entries,
        recovered as f64 / t.entries as f64 * 100.0,
        t.mem_mib,
        t.disk_mib,
        t.block_mib,
        t.submit_queue_mib,
        t.insert_delay_ms,
    );
}

#[tokio::main]
async fn main() {
    // 256 entries of 1 MiB into a 1 GiB disk tier: everything fits many times over, so nothing
    // here is an eviction for capacity.
    let base = Trial {
        label: "base",
        use_insert: true,
        policy: HybridCachePolicy::WriteOnInsertion,
        entries: 256,
        entry_kib: 1024,
        mem_mib: 64,
        disk_mib: 1024,
        block_mib: 16,
        submit_queue_mib: 16,
        insert_delay_ms: 0,
    };

    // If entries are dropped because the writer outruns the flusher, slowing the writer recovers
    // them. If write rate makes no difference, the cause is elsewhere.
    for insert_delay_ms in [0, 2, 10, 25] {
        trial(Trial {
            label: "rate",
            insert_delay_ms,
            ..base
        })
        .await;
    }

    // Does a memory tier large enough to hold everything change the outcome?
    trial(Trial {
        label: "bigmem",
        mem_mib: 512,
        ..base
    })
    .await;
}
