//! Differential and behavioural tests for the M0 spike (TASK-03 phase 4).
//!
//! The differential argument is the whole correctness case for a caching proxy: for random
//! objects and random ranges, the bytes a client receives through the cache must equal the bytes
//! the origin would have sent. Content is generated, not stored, so this is affordable to run
//! over large objects.

use std::{sync::Arc, time::Duration};

use cachic::spike::{
    proxy::{SpikeConfig, SpikeProxy},
    store::StoreConfig,
};
use cachic_testkit::{
    content,
    mockcdn::{Config as CdnConfig, MockCdn, RangeBehaviour},
};

const SLICE: u32 = 64 * 1024;

/// Deterministic RNG, so a failure reproduces from the seed printed in the assertion.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let base = std::env::var("CACHIC_TEST_TMP").unwrap_or_else(|_| "/tmp".into());
        let path = std::path::Path::new(&base).join(format!(
            "cachic-spike-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn harness(tag: &str, cdn: CdnConfig) -> (MockCdn, SpikeProxy, Scratch) {
    let origin = MockCdn::start(cdn).await.unwrap();
    let scratch = Scratch::new(tag);
    let mut config = SpikeConfig::new(origin.base_url(), scratch.path());
    config.slice_size = SLICE;
    config.readahead = 4;
    config.store = StoreConfig {
        memory_bytes: 8 * 1024 * 1024,
        disk_bytes: 256 * 1024 * 1024,
        block_bytes: 4 * 1024 * 1024,
    };
    let proxy = SpikeProxy::start(config).await.unwrap();
    (origin, proxy, scratch)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap()
}

#[tokio::test]
async fn full_object_matches_the_origin() {
    let (_cdn, proxy, _s) = harness("full", CdnConfig::default()).await;
    let size = 300_000u64;
    let path = MockCdn::object_path("full-object", size);

    let response = client()
        .get(format!("{}{}", proxy.base_url(), path))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-cache"], "MISS");
    let body = response.bytes().await.unwrap();

    assert_eq!(body.len() as u64, size);
    assert_eq!(
        body.as_ref(),
        content::range(content::seed_for("full-object"), 0, size as usize).as_slice()
    );
    proxy.close().await.unwrap();
}

#[tokio::test]
async fn random_ranges_match_the_origin_cold_and_warm() {
    let (_cdn, proxy, _s) = harness("ranges", CdnConfig::default()).await;
    let client = client();
    let seed = 0xC0FF_EE00_1234_5678u64;
    let mut rng = Rng(seed);

    // Deliberately not a multiple of the slice size, so the last slice is short.
    let size = 5 * SLICE as u64 + 1_234;

    for pass in 0..2 {
        for iteration in 0..40 {
            let name = format!("obj-{}", rng.below(6));
            let path = MockCdn::object_path(&name, size);
            let start = rng.below(size);
            let len = 1 + rng.below(size - start);
            let end = start + len - 1;

            let response = client
                .get(format!("{}{}", proxy.base_url(), path))
                .header("Range", format!("bytes={start}-{end}"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                206,
                "seed {seed:#x} pass {pass} iteration {iteration}"
            );
            assert_eq!(
                response.headers()["content-range"],
                format!("bytes {start}-{end}/{size}")
            );
            let body = response.bytes().await.unwrap();
            let expected = content::range(content::seed_for(&name), start, len as usize);
            assert_eq!(
                body.as_ref(),
                expected.as_slice(),
                "seed {seed:#x} pass {pass} iteration {iteration}: range {start}-{end} of {name}"
            );
        }
    }
    proxy.close().await.unwrap();
}

#[tokio::test]
async fn concurrent_clients_share_one_upstream_fetch_per_slice() {
    // FR-30, end to end: 24 clients asking for the same cold object must not multiply upstream
    // traffic. This is the behaviour nginx's proxy_cache_lock does not give.
    let (cdn, proxy, _s) = harness(
        "coalesce",
        CdnConfig {
            // Hold each slice open long enough for the other clients to pile up behind it.
            first_byte_delay: Some(Duration::from_millis(60)),
            ..CdnConfig::default()
        },
    )
    .await;

    let size = 8 * SLICE as u64;
    let path = MockCdn::object_path("hot-object", size);
    let slices = size / SLICE as u64;
    let proxy = Arc::new(proxy);
    let client = client();

    let mut handles = Vec::new();
    for _ in 0..24 {
        let client = client.clone();
        let url = format!("{}{}", proxy.base_url(), path);
        handles.push(tokio::spawn(async move {
            let r = client.get(url).send().await.unwrap();
            assert_eq!(r.status(), 200);
            r.bytes().await.unwrap()
        }));
    }

    let expected = content::range(content::seed_for("hot-object"), 0, size as usize);
    for h in handles {
        let body = h.await.unwrap();
        assert_eq!(body.as_ref(), expected.as_slice());
    }

    let upstream = cdn.stats().requests();
    // One probe plus one fetch per slice is the floor. Allow a small margin for the race where
    // two clients both miss before the first fetch registers.
    assert!(
        upstream <= slices + 4,
        "24 clients caused {upstream} upstream requests for {slices} slices; coalescing failed"
    );
    assert!(
        cdn.stats().bytes_served() <= size + SLICE as u64 * 2,
        "upstream served {} bytes for a {size}-byte object",
        cdn.stats().bytes_served()
    );
    proxy.close().await.unwrap();
}

#[tokio::test]
async fn second_request_is_served_from_cache() {
    let (cdn, proxy, _s) = harness("warm", CdnConfig::default()).await;
    let size = 3 * SLICE as u64;
    let path = MockCdn::object_path("warm-object", size);
    let client = client();

    let first = client
        .get(format!("{}{}", proxy.base_url(), path))
        .send()
        .await
        .unwrap();
    assert_eq!(first.headers()["x-cache"], "MISS");
    let first_body = first.bytes().await.unwrap();
    let upstream_after_first = cdn.stats().requests();

    let second = client
        .get(format!("{}{}", proxy.base_url(), path))
        .send()
        .await
        .unwrap();
    assert_eq!(second.headers()["x-cache"], "HIT");
    let second_body = second.bytes().await.unwrap();

    assert_eq!(first_body, second_body);
    assert_eq!(
        cdn.stats().requests(),
        upstream_after_first,
        "a warm read must not touch the origin"
    );
    proxy.close().await.unwrap();
}

#[tokio::test]
async fn range_ignoring_origin_still_serves_correct_ranges() {
    // FR-13: the origin returns 200 for a Range request. The spike fills the object whole and
    // still satisfies the client's range.
    let (_cdn, proxy, _s) = harness(
        "noranges",
        CdnConfig {
            range_behaviour: RangeBehaviour::Ignore,
            ..CdnConfig::default()
        },
    )
    .await;

    let size = 2 * SLICE as u64 + 999;
    let path = MockCdn::object_path("stubborn", size);
    let start = SLICE as u64 + 100;
    let end = start + 5_000;

    let response = client()
        .get(format!("{}{}", proxy.base_url(), path))
        .header("Range", format!("bytes={start}-{end}"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), 206);
    let body = response.bytes().await.unwrap();
    let expected = content::range(
        content::seed_for("stubborn"),
        start,
        (end - start + 1) as usize,
    );
    assert_eq!(body.as_ref(), expected.as_slice());
    proxy.close().await.unwrap();
}

#[tokio::test]
async fn suffix_and_open_ended_ranges_resolve() {
    let (_cdn, proxy, _s) = harness("suffix", CdnConfig::default()).await;
    let size = 100_000u64;
    let path = MockCdn::object_path("tail", size);
    let seed = content::seed_for("tail");
    let client = client();

    let r = client
        .get(format!("{}{}", proxy.base_url(), path))
        .header("Range", "bytes=-1000")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(
        r.headers()["content-range"],
        format!("bytes 99000-99999/{size}")
    );
    assert_eq!(
        r.bytes().await.unwrap().as_ref(),
        content::range(seed, 99_000, 1_000).as_slice()
    );

    let r = client
        .get(format!("{}{}", proxy.base_url(), path))
        .header("Range", "bytes=99500-")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(
        r.bytes().await.unwrap().as_ref(),
        content::range(seed, 99_500, 500).as_slice()
    );
    proxy.close().await.unwrap();
}

#[tokio::test]
async fn unsatisfiable_range_returns_416() {
    let (_cdn, proxy, _s) = harness("416", CdnConfig::default()).await;
    let size = 10_000u64;
    let path = MockCdn::object_path("small", size);

    let r = client()
        .get(format!("{}{}", proxy.base_url(), path))
        .header("Range", "bytes=50000-60000")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 416);
    assert_eq!(r.headers()["content-range"], format!("bytes */{size}"));
    proxy.close().await.unwrap();
}

#[tokio::test]
async fn multi_range_is_answered_with_the_whole_object() {
    // RFC 9110 permits this, and it is what monolithic effectively does (FR-11).
    let (_cdn, proxy, _s) = harness("multi", CdnConfig::default()).await;
    let size = 50_000u64;
    let path = MockCdn::object_path("multi", size);

    let r = client()
        .get(format!("{}{}", proxy.base_url(), path))
        .header("Range", "bytes=0-99,200-299")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.bytes().await.unwrap().len() as u64, size);
    proxy.close().await.unwrap();
}

#[tokio::test]
async fn head_returns_metadata_without_a_body() {
    let (_cdn, proxy, _s) = harness("head", CdnConfig::default()).await;
    let size = 123_456u64;
    let path = MockCdn::object_path("headable", size);

    let r = client()
        .head(format!("{}{}", proxy.base_url(), path))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-length"], size.to_string());
    assert!(r.bytes().await.unwrap().is_empty());
    proxy.close().await.unwrap();
}

#[tokio::test]
async fn heartbeat_answers_the_ecosystem_probe() {
    // Prefill tools use this to decide whether a cache is present (FR-07).
    let (_cdn, proxy, _s) = harness("heartbeat", CdnConfig::default()).await;
    let r = client()
        .get(format!("{}/lancache-heartbeat", proxy.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert!(r.headers().contains_key("x-lancache-processed-by"));
    proxy.close().await.unwrap();
}

#[tokio::test]
async fn ranges_spanning_the_short_final_slice_are_correct() {
    // The final slice of an object is short unless the size is an exact multiple, which is the
    // classic off-by-one in slice arithmetic.
    let (_cdn, proxy, _s) = harness("tailslice", CdnConfig::default()).await;
    let size = 2 * SLICE as u64 + 7;
    let path = MockCdn::object_path("ragged", size);
    let seed = content::seed_for("ragged");
    let client = client();

    for (start, end) in [
        (size - 1, size - 1),
        (size - 8, size - 1),
        (2 * SLICE as u64 - 1, size - 1),
        (0, size - 1),
    ] {
        let r = client
            .get(format!("{}{}", proxy.base_url(), path))
            .header("Range", format!("bytes={start}-{end}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 206, "range {start}-{end}");
        let body = r.bytes().await.unwrap();
        assert_eq!(
            body.as_ref(),
            content::range(seed, start, (end - start + 1) as usize).as_slice(),
            "range {start}-{end}"
        );
    }
    proxy.close().await.unwrap();
}
