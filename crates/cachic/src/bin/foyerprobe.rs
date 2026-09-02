//! Minimal foyer persistence probe.
//!
//! Answers one question and nothing else: after a clean close and reopen, how many entries come
//! back? It talks to foyer directly, with no cachic types in the way, so a failure here cannot be
//! blamed on our slice codec or store wrapper.
//!
//! It compares two ways of populating the cache, because that is the most likely way for us to be
//! holding it wrong:
//!
//! - `insert`, the ordinary write API
//! - `get_or_fetch`, which is how the proxy populates the cache as a side effect of serving

use std::time::Instant;

use foyer::{
    BlockEngineConfig, DeviceBuilder, FsDeviceBuilder, HybridCache, HybridCacheBuilder,
    HybridCachePolicy, PsyncIoEngineConfig,
};

const MIB: usize = 1024 * 1024;

async fn open(
    dir: &std::path::Path,
    mem_mib: usize,
    disk_mib: usize,
    block_mib: usize,
    policy: HybridCachePolicy,
) -> HybridCache<u64, Vec<u8>> {
    let device = FsDeviceBuilder::new(dir)
        .with_capacity(disk_mib * MIB)
        .with_direct(false)
        .build()
        .unwrap();
    HybridCacheBuilder::new()
        .with_name("probe")
        .with_policy(policy)
        .with_flush_on_close(true)
        .memory(mem_mib * MIB)
        .with_weighter(|_k: &u64, v: &Vec<u8>| 8 + v.len())
        .storage()
        .with_io_engine_config(PsyncIoEngineConfig::new())
        .with_engine_config(BlockEngineConfig::new(device).with_block_size(block_mib * MIB))
        .build()
        .await
        .unwrap()
}

/// One persistence trial: populate, close, reopen, count what came back.
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
}

async fn trial(t: Trial) {
    let Trial {
        label,
        use_insert,
        policy,
        entries,
        entry_kib,
        mem_mib,
        disk_mib,
        block_mib,
    } = t;
    let dir = std::path::PathBuf::from(format!("/root/.cache/foyerprobe-{label}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    {
        let cache = open(&dir, mem_mib, disk_mib, block_mib, policy).await;
        for i in 0..entries {
            let payload = vec![(i % 251) as u8; entry_kib * 1024];
            if use_insert {
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
        let start = Instant::now();
        cache.close().await.unwrap();
        println!("{label}: close took {:.3}s", start.elapsed().as_secs_f64());
    }

    let cache = open(&dir, mem_mib, disk_mib, block_mib, policy).await;
    let mut found = 0usize;
    for i in 0..entries {
        if cache.get(&(i as u64)).await.unwrap().is_some() {
            found += 1;
        }
    }
    let data_mib = entries * entry_kib / 1024;
    println!(
        "{label}: populated with {}, policy {:?} -> recovered {found}/{entries} ({:.1}%) [data {data_mib} MiB, mem {mem_mib} MiB, disk {disk_mib} MiB, block {block_mib} MiB]",
        if use_insert { "insert" } else { "get_or_fetch" },
        policy,
        found as f64 / entries as f64 * 100.0
    );
    let _ = cache.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::main]
async fn main() {
    // 256 entries of 1 MiB = 256 MiB of data, into a 1 GiB disk tier. Everything fits with room
    // to spare, so anything missing is not an eviction.
    let big = Trial {
        label: "insert-woi",
        use_insert: true,
        policy: HybridCachePolicy::WriteOnInsertion,
        entries: 256,
        entry_kib: 1024,
        mem_mib: 64,
        disk_mib: 1024,
        block_mib: 16,
    };

    trial(big).await;
    trial(Trial {
        label: "fetch-woi",
        use_insert: false,
        ..big
    })
    .await;
    trial(Trial {
        label: "insert-woe",
        policy: HybridCachePolicy::WriteOnEviction,
        ..big
    })
    .await;
    trial(Trial {
        label: "fetch-woe",
        use_insert: false,
        policy: HybridCachePolicy::WriteOnEviction,
        ..big
    })
    .await;
    // Small entries, to separate "large entry handling" from "persistence".
    trial(Trial {
        label: "insert-small",
        entries: 4096,
        entry_kib: 4,
        mem_mib: 4,
        disk_mib: 256,
        block_mib: 4,
        ..big
    })
    .await;
}
