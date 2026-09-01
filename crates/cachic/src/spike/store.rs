//! Thin wrapper over foyer's hybrid cache.
//!
//! The wrapper exists for the reason plan section 10 gives: foyer's API is the largest external
//! risk in the design, so it is kept behind one boundary. Everything above this module speaks in
//! slices; only this module knows foyer exists.

use std::path::Path;

use foyer::{
    BlockEngineConfig, DeviceBuilder, FsDeviceBuilder, HybridCache, HybridCacheBuilder,
    HybridCachePolicy, PsyncIoEngineConfig,
};

use super::slice::{SliceKey, SliceValue};

/// Store sizing. Both tiers are hard caps (FR-40).
#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// Memory tier capacity in bytes (`CACHE_MEM_SIZE`).
    pub memory_bytes: usize,
    /// Disk tier capacity in bytes (`CACHE_DISK_SIZE`).
    pub disk_bytes: usize,
    /// Disk block size. Must comfortably exceed one encoded slice.
    pub block_bytes: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            disk_bytes: 1024 * 1024 * 1024,
            block_bytes: 16 * 1024 * 1024,
        }
    }
}

/// The slice store.
#[derive(Clone)]
pub struct SpikeStore {
    inner: HybridCache<SliceKey, SliceValue>,
}

impl SpikeStore {
    pub async fn open(dir: &Path, config: &StoreConfig) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let device = FsDeviceBuilder::new(dir)
            .with_capacity(config.disk_bytes)
            .build()?;
        let inner: HybridCache<SliceKey, SliceValue> = HybridCacheBuilder::new()
            .with_name("cachic-spike")
            // Slices are written as they are fetched, not only when evicted from memory: a
            // restart must not lose everything the memory tier was holding (FR-43).
            .with_policy(HybridCachePolicy::WriteOnInsertion)
            .memory(config.memory_bytes)
            // Weight by encoded size so the memory cap is a real byte cap, not an entry count.
            .with_weighter(|_k: &SliceKey, v: &SliceValue| SliceKey::ENCODED_LEN + v.payload.len())
            .storage()
            .with_io_engine_config(PsyncIoEngineConfig::new())
            .with_engine_config(BlockEngineConfig::new(device).with_block_size(config.block_bytes))
            .build()
            .await?;
        Ok(Self { inner })
    }

    /// Fetch a slice, coalescing concurrent misses for the same key (FR-30).
    ///
    /// `fetch` is only polled on a miss. Every concurrent caller for the same key shares the one
    /// in-flight fetch; this is the behaviour being bought over nginx's `proxy_cache_lock`, where
    /// waiters block on a lock rather than streaming the in-flight fill.
    pub async fn get_or_fetch<F, Fut>(&self, key: SliceKey, fetch: F) -> anyhow::Result<SliceValue>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<SliceValue>> + Send + 'static,
    {
        let entry = self.inner.get_or_fetch(&key, fetch).await?;
        Ok(entry.value().clone())
    }

    /// Whether a slice is already resident, without fetching it.
    ///
    /// Used only to classify `X-Cache`; it is inherently racy and must never gate correctness.
    pub fn contains(&self, key: &SliceKey) -> bool {
        self.inner.contains(key)
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        self.inner.close().await?;
        Ok(())
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
    use crate::spike::slice::{object_id, SliceHeader};

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

    async fn store(dir: &Path) -> SpikeStore {
        SpikeStore::open(
            dir,
            &StoreConfig {
                memory_bytes: 4 * 1024 * 1024,
                disk_bytes: 64 * 1024 * 1024,
                block_bytes: 4 * 1024 * 1024,
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn stores_and_returns_slices() {
        let dir = tempdir();
        let store = store(dir.path()).await;
        let key = SliceKey::new(object_id("/a"), 0, 0);
        let got = store
            .get_or_fetch(key, || async { Ok(value(b"payload")) })
            .await
            .unwrap();
        assert_eq!(got.payload, Bytes::from_static(b"payload"));
        assert!(store.contains(&key));
        store.close().await.unwrap();
    }

    #[tokio::test]
    async fn does_not_refetch_a_resident_slice() {
        let dir = tempdir();
        let store = store(dir.path()).await;
        let key = SliceKey::new(object_id("/b"), 0, 0);
        let calls = Arc::new(AtomicU64::new(0));

        for _ in 0..5 {
            let c = calls.clone();
            store
                .get_or_fetch(key, move || async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    Ok(value(b"once"))
                })
                .await
                .unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        store.close().await.unwrap();
    }

    #[tokio::test]
    async fn coalesces_concurrent_misses() {
        // The FR-30 claim, asserted directly: N concurrent callers for one cold key produce one
        // upstream fetch, and all of them get the bytes.
        let dir = tempdir();
        let store = store(dir.path()).await;
        let key = SliceKey::new(object_id("/c"), 0, 7);
        let calls = Arc::new(AtomicU64::new(0));

        let mut handles = Vec::new();
        for _ in 0..32 {
            let store = store.clone();
            let calls = calls.clone();
            handles.push(tokio::spawn(async move {
                store
                    .get_or_fetch(key, move || async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        // Hold the fetch open so the other callers pile up behind it.
                        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                        Ok(value(b"shared"))
                    })
                    .await
            }));
        }
        for h in handles {
            let v = h.await.unwrap().unwrap();
            assert_eq!(v.payload, Bytes::from_static(b"shared"));
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "32 concurrent misses must produce exactly one fetch"
        );
        store.close().await.unwrap();
    }

    #[tokio::test]
    async fn distinct_generations_are_distinct_slices() {
        // Generation is part of the key, which is what makes invalidation atomic (FR-14).
        let dir = tempdir();
        let store = store(dir.path()).await;
        let object = object_id("/d");
        let old = SliceKey::new(object, 0, 0);
        let new = SliceKey::new(object, 1, 0);

        store
            .get_or_fetch(old, || async { Ok(value(b"old")) })
            .await
            .unwrap();
        let got = store
            .get_or_fetch(new, || async { Ok(value(b"new")) })
            .await
            .unwrap();
        assert_eq!(got.payload, Bytes::from_static(b"new"));
        store.close().await.unwrap();
    }

    #[tokio::test]
    async fn fetch_errors_propagate_without_poisoning_the_key() {
        let dir = tempdir();
        let store = store(dir.path()).await;
        let key = SliceKey::new(object_id("/e"), 0, 0);

        let err = store
            .get_or_fetch(key, || async { anyhow::bail!("upstream exploded") })
            .await;
        assert!(err.is_err());
        assert!(!store.contains(&key));

        // A later attempt must still be able to fill the key.
        let ok = store
            .get_or_fetch(key, || async { Ok(value(b"recovered")) })
            .await
            .unwrap();
        assert_eq!(ok.payload, Bytes::from_static(b"recovered"));
        store.close().await.unwrap();
    }

    /// Minimal scratch directory helper, so the spike does not pull in `tempfile`.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TempDir {
        let base = std::env::var("CACHIC_TEST_TMP").unwrap_or_else(|_| "/tmp".into());
        let unique = format!(
            "cachic-spike-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::path::Path::new(&base).join(unique);
        std::fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }
}
