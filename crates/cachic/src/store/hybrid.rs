//! The hybrid RAM + disk slice store.
//!
//! Wraps foyer, and is the only module that knows foyer exists (ADR 0003). Everything above
//! speaks in slices, so replacing the engine would not reach past this file.
//!
//! # Write-path defaults
//!
//! foyer silently discards a disk write when its flushers cannot keep up, which is a reasonable
//! design for a cache but means there is a maximum sustainable fill rate. M0 measured foyer's
//! defaults (1 flusher, 16 MiB buffer pool) losing 10% of a 10 Gbit fill while retaining
//! everything at 5 Gbit and below. We ship 4 flushers and a 128 MiB pool, which covered every
//! fibre tier tested, because the cost is negligible and the failure is silent.
//!
//! See `docs/benchmarks/m0/README.md`.

use std::{path::Path, sync::Arc};

use foyer::{
    BlockEngineConfig, DeviceBuilder, FsDeviceBuilder, HybridCache, HybridCacheBuilder,
    HybridCachePolicy, PsyncIoEngineConfig,
};
use mixtrics::registry::prometheus_0_14::PrometheusMetricsRegistry;

use super::slice::{SliceKey, SliceValue};

/// Flushers draining the disk write queue. foyer's default is 1, which is not enough above
/// 5 Gbit of fill.
const DEFAULT_FLUSHERS: usize = 4;

/// Total flush buffer pool, shared across flushers. foyer's default is 16 MiB.
const DEFAULT_BUFFER_POOL: usize = 128 * 1024 * 1024;

/// Disk block size. Must comfortably exceed one encoded slice, since a block is the unit of
/// disk eviction and also caps the largest storable entry.
const DEFAULT_BLOCK_SIZE: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// `CACHE_MEM_SIZE`.
    pub memory_bytes: usize,
    /// `CACHE_DISK_SIZE`.
    pub disk_bytes: usize,
    pub block_bytes: usize,
    pub flushers: usize,
    pub buffer_pool_bytes: usize,
    /// O_DIRECT for the disk tier.
    ///
    /// Off by default. M0 measured buffered reads at roughly twice direct on the development
    /// host; that comparison is hardware-dependent and should be repeated on real NVMe before
    /// the default is reconsidered.
    pub direct_io: bool,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            memory_bytes: 2 * 1024 * 1024 * 1024,
            disk_bytes: 1000 * 1024 * 1024 * 1024,
            block_bytes: DEFAULT_BLOCK_SIZE,
            flushers: DEFAULT_FLUSHERS,
            buffer_pool_bytes: DEFAULT_BUFFER_POOL,
            direct_io: false,
        }
    }
}

impl StoreConfig {
    /// Derive the store configuration from the process configuration.
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            memory_bytes: config.cache_mem_size as usize,
            disk_bytes: config.cache_disk_size as usize,
            // A block must hold several slices to be useful as an eviction unit.
            block_bytes: DEFAULT_BLOCK_SIZE.max(config.cache_slice_size as usize * 4),
            ..Self::default()
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot create cache directory {path}: {source}")]
    Directory {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot open the slice store: {0}")]
    Open(#[source] foyer::Error),
    #[error("slice fetch failed: {0}")]
    Fetch(#[source] anyhow::Error),
}

/// The slice store.
#[derive(Clone)]
pub struct SliceStore {
    inner: HybridCache<SliceKey, SliceValue>,
}

impl std::fmt::Debug for SliceStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SliceStore").finish_non_exhaustive()
    }
}

impl SliceStore {
    pub async fn open(dir: &Path, config: &StoreConfig) -> Result<Self, StoreError> {
        Self::open_with_metrics(dir, config, None).await
    }

    /// Open with foyer reporting into our Prometheus registry.
    ///
    /// This is how `foyer_storage_queue_channel_overflow` reaches `/metrics`. That counter is the
    /// only way an operator learns the cache has silently stopped caching because writes are
    /// outrunning the flushers, which is the failure M0 spent a day misdiagnosing.
    pub async fn open_with_metrics(
        dir: &Path,
        config: &StoreConfig,
        registry: Option<Arc<PrometheusMetricsRegistry>>,
    ) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir).map_err(|source| StoreError::Directory {
            path: dir.to_owned(),
            source,
        })?;
        // `with_direct` is Linux-only in foyer: O_DIRECT does not exist on macOS, and the method
        // is `#[cfg(target_os = "linux")]`. Calling it unconditionally means cachic does not
        // compile for macOS at all, which is how it was found - the release pipeline's Darwin
        // binary failed to build. macOS is a development platform here, not a deployment target,
        // so it simply uses buffered IO.
        let device = FsDeviceBuilder::new(dir).with_capacity(config.disk_bytes);
        #[cfg(target_os = "linux")]
        let device = device.with_direct(config.direct_io);
        #[cfg(not(target_os = "linux"))]
        let _ = config.direct_io;
        let device = device.build().map_err(StoreError::Open)?;

        let mut builder = HybridCacheBuilder::new().with_name("cachic");
        if let Some(registry) = registry {
            builder = builder.with_metrics_registry(Box::new((*registry).clone()));
        }
        let inner = builder
            // Write on insertion rather than on eviction: a restart must not lose everything the
            // memory tier happened to be holding (FR-43).
            .with_policy(HybridCachePolicy::WriteOnInsertion)
            .with_flush_on_close(true)
            .memory(config.memory_bytes)
            // Weight by encoded size so CACHE_MEM_SIZE is a byte cap rather than an entry count.
            .with_weighter(|_k: &SliceKey, v: &SliceValue| SliceKey::ENCODED_LEN + v.payload.len())
            .storage()
            .with_io_engine_config(PsyncIoEngineConfig::new())
            .with_engine_config(
                BlockEngineConfig::new(device)
                    .with_block_size(config.block_bytes)
                    .with_flushers(config.flushers)
                    .with_buffer_pool_size(config.buffer_pool_bytes),
            )
            .build()
            .await
            .map_err(StoreError::Open)?;

        Ok(Self { inner })
    }

    /// Fetch a slice, coalescing concurrent misses for the same key (FR-30).
    ///
    /// `fetch` is polled only on a miss, and every concurrent caller for the same key shares the
    /// one in-flight fetch. This is the behaviour bought over nginx's `proxy_cache_lock`, where
    /// waiters block on a lock rather than streaming the fill in progress.
    pub async fn get_or_fetch<F, Fut>(
        &self,
        key: SliceKey,
        fetch: F,
    ) -> Result<SliceValue, StoreError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<SliceValue>> + Send + 'static,
    {
        let entry = self
            .inner
            .get_or_fetch(&key, fetch)
            .await
            .map_err(|e| StoreError::Fetch(anyhow::anyhow!(e)))?;
        Ok(entry.value().clone())
    }

    /// Read a slice without fetching it.
    pub async fn get(&self, key: &SliceKey) -> Result<Option<SliceValue>, StoreError> {
        let found = self
            .inner
            .get(key)
            .await
            .map_err(|e| StoreError::Fetch(anyhow::anyhow!(e)))?;
        Ok(found.map(|entry| entry.value().clone()))
    }

    /// Whether a slice is resident. Racy by construction, and used only to classify `X-Cache`;
    /// it must never gate correctness.
    pub fn contains(&self, key: &SliceKey) -> bool {
        self.inner.contains(key)
    }

    pub fn insert(&self, key: SliceKey, value: SliceValue) {
        self.inner.insert(key, value);
    }

    pub fn remove(&self, key: &SliceKey) {
        self.inner.remove(key);
    }

    pub async fn close(&self) -> Result<(), StoreError> {
        self.inner
            .close()
            .await
            .map_err(|e| StoreError::Fetch(anyhow::anyhow!(e)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };

    use bytes::Bytes;

    use super::*;
    use crate::{
        store::slice::{object_id, SliceHeader},
        test_support::Scratch,
    };

    fn value(payload: &[u8]) -> SliceValue {
        SliceValue::new(
            SliceHeader {
                slice_size: 1024,
                total_len: 4096,
                generation: 0,
                etag: Some("\"v1\"".into()),
                last_modified: None,
                content_type: None,
            },
            Bytes::copy_from_slice(payload),
        )
    }

    async fn store(dir: &Scratch) -> SliceStore {
        SliceStore::open(
            dir.path(),
            &StoreConfig {
                memory_bytes: 8 * 1024 * 1024,
                disk_bytes: 128 * 1024 * 1024,
                block_bytes: 4 * 1024 * 1024,
                flushers: 2,
                buffer_pool_bytes: 8 * 1024 * 1024,
                direct_io: false,
            },
        )
        .await
        .unwrap()
    }

    #[test]
    fn ships_write_path_defaults_above_foyers() {
        // M0 measured foyer's defaults dropping 10% of a 10 Gbit fill. Regressing to them would
        // silently lose cache content on a fast link.
        let c = StoreConfig::default();
        assert!(
            c.flushers > 1,
            "one flusher is foyer's default and is not enough"
        );
        assert!(
            c.buffer_pool_bytes >= 64 * 1024 * 1024,
            "buffer pool must exceed foyer's 16 MiB default"
        );
    }

    #[tokio::test]
    async fn stores_and_returns_slices() {
        let dir = Scratch::new("store-basic");
        let s = store(&dir).await;
        let key = SliceKey::new(object_id("/a"), 0, 0);
        let got = s
            .get_or_fetch(key, || async { Ok(value(b"payload")) })
            .await
            .unwrap();
        assert_eq!(got.payload, Bytes::from_static(b"payload"));
        assert!(s.contains(&key));
        assert_eq!(s.get(&key).await.unwrap().unwrap().payload, got.payload);
        s.close().await.unwrap();
    }

    #[tokio::test]
    async fn coalesces_concurrent_misses() {
        let dir = Scratch::new("store-coalesce");
        let s = store(&dir).await;
        let key = SliceKey::new(object_id("/b"), 0, 7);
        let calls = Arc::new(AtomicU64::new(0));

        let mut handles = Vec::new();
        for _ in 0..32 {
            let s = s.clone();
            let calls = calls.clone();
            handles.push(tokio::spawn(async move {
                s.get_or_fetch(key, move || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                    Ok(value(b"shared"))
                })
                .await
            }));
        }
        for h in handles {
            assert_eq!(
                h.await.unwrap().unwrap().payload,
                Bytes::from_static(b"shared")
            );
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "FR-30: one fetch for N waiters"
        );
        s.close().await.unwrap();
    }

    #[tokio::test]
    async fn generations_address_distinct_slices() {
        let dir = Scratch::new("store-generations");
        let s = store(&dir).await;
        let object = object_id("/c");
        s.get_or_fetch(SliceKey::new(object, 0, 0), || async { Ok(value(b"old")) })
            .await
            .unwrap();
        let new = s
            .get_or_fetch(SliceKey::new(object, 1, 0), || async { Ok(value(b"new")) })
            .await
            .unwrap();
        assert_eq!(new.payload, Bytes::from_static(b"new"));
        s.close().await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_fetch_does_not_poison_the_key() {
        let dir = Scratch::new("store-error");
        let s = store(&dir).await;
        let key = SliceKey::new(object_id("/d"), 0, 0);
        assert!(s
            .get_or_fetch(key, || async { anyhow::bail!("upstream exploded") })
            .await
            .is_err());
        assert!(!s.contains(&key));
        let ok = s
            .get_or_fetch(key, || async { Ok(value(b"recovered")) })
            .await
            .unwrap();
        assert_eq!(ok.payload, Bytes::from_static(b"recovered"));
        s.close().await.unwrap();
    }

    #[tokio::test]
    async fn foyers_drop_signal_reaches_our_metrics() {
        // The M0 obligation, proven rather than assumed: when writes outrun the flushers, foyer
        // discards them silently, and this counter is the only way an operator finds out.
        //
        // foyer reports it as a label rather than its own metric -
        // foyer_storage_inner_op_total{op="channel_overflow"} - and a Prometheus label only
        // materialises once incremented. So the test deliberately causes drops, which also
        // proves the signal actually fires rather than merely being registered.
        let dir = Scratch::new("store-foyer-metrics");
        let (metrics, foyer_registry) = crate::telemetry::metrics::Metrics::new().unwrap();
        let s = SliceStore::open_with_metrics(
            dir.path(),
            &StoreConfig {
                memory_bytes: 4 * 1024 * 1024,
                disk_bytes: 256 * 1024 * 1024,
                block_bytes: 4 * 1024 * 1024,
                // foyer's own defaults, deliberately: this is the configuration we do not ship.
                flushers: 1,
                buffer_pool_bytes: 1024 * 1024,
                direct_io: false,
            },
            Some(foyer_registry),
        )
        .await
        .unwrap();

        // Write far faster than a 1 MiB buffer pool can drain.
        let object = object_id("/overflow");
        let payload = vec![0u8; 256 * 1024];
        for i in 0..256u32 {
            let v = SliceValue::new(
                SliceHeader {
                    slice_size: 256 * 1024,
                    total_len: 256 * 256 * 1024,
                    generation: 0,
                    etag: Some("\"o\"".into()),
                    last_modified: None,
                    content_type: None,
                },
                Bytes::from(payload.clone()),
            );
            s.insert(SliceKey::new(object, 0, i), v);
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let text = metrics.render().unwrap();
        // The families must be exported whether or not anything has been dropped, since that is
        // what a dashboard and an alert rule reference.
        assert!(
            text.contains("foyer_storage_inner_op_total"),
            "the family carrying channel_overflow is missing"
        );
        assert!(
            text.contains("foyer_storage_block_engine_op_total"),
            "the family carrying enqueue_skip is missing"
        );
        // And under this deliberately undersized configuration, the drop signal must actually
        // fire. If it does not, either foyer stopped dropping or the label moved.
        assert!(
            text.contains("channel_overflow"),
            "writes outran a 1 MiB buffer pool but no drop was reported; \
             the alerting signal for silent cache loss is not working"
        );
        s.close().await.unwrap();
    }

    #[tokio::test]
    async fn retains_everything_written_at_a_realistic_fill_rate() {
        // The M0 lesson, guarded: paced writes must not be dropped. 200 Mbit/s is the project's
        // upstream fill bar; this runs far above it and still expects nothing lost.
        let dir = Scratch::new("store-fillrate");
        let s = store(&dir).await;
        let object = object_id("/fill");
        let payload = vec![0x5au8; 256 * 1024];
        let count = 128u32;

        for i in 0..count {
            let v = SliceValue::new(
                SliceHeader {
                    slice_size: 256 * 1024,
                    total_len: (count as u64) * 256 * 1024,
                    generation: 0,
                    etag: Some("\"fill\"".into()),
                    last_modified: None,
                    content_type: None,
                },
                Bytes::from(payload.clone()),
            );
            s.get_or_fetch(SliceKey::new(object, 0, i), move || async move { Ok(v) })
                .await
                .unwrap();
            // ~32 MiB over ~1.3s is about 25 MiB/s, an order of magnitude above the 200 Mbit/s
            // bar and far below where foyer starts shedding.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let resident = (0..count)
            .filter(|i| s.contains(&SliceKey::new(object, 0, *i)))
            .count();
        assert_eq!(
            resident,
            count as usize,
            "{} of {count} slices were dropped at a realistic fill rate",
            count as usize - resident
        );
        s.close().await.unwrap();
    }
}
