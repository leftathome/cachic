//! End-to-end tests for the M1 proxy (TASK-12).
//!
//! The differential test is the correctness argument for the whole project: for random objects
//! and random ranges, the bytes a client receives through the cache must equal the bytes the
//! origin would have sent.

use std::{collections::HashMap, sync::Arc, time::Duration};

use cachic::{
    config::rules::Rules,
    orchestrator::Orchestrator,
    proxy::server::{Server, ServerConfig},
    services::{domains::DomainList, key::CompiledRule, matcher::Matcher},
    store::{
        hybrid::{SliceStore, StoreConfig},
        index::ObjectIndex,
    },
    test_support::Scratch,
    upstream::{
        client::{ClientConfig, UpstreamClient},
        resolver::UpstreamResolver,
    },
};
use cachic_testkit::{
    content,
    mockcdn::{Config as CdnConfig, MockCdn},
};

const SLICE: u32 = 64 * 1024;

struct Harness {
    _scratch: Scratch,
    origin: MockCdn,
    server: Server,
    orchestrator: Arc<Orchestrator>,
}

impl Harness {
    async fn start(tag: &str, cdn: CdnConfig) -> Self {
        let origin = MockCdn::start(cdn).await.unwrap();
        let scratch = Scratch::new(tag);

        let store = SliceStore::open(
            &scratch.path().join("slices"),
            &StoreConfig {
                memory_bytes: 64 * 1024 * 1024,
                disk_bytes: 256 * 1024 * 1024,
                block_bytes: 4 * 1024 * 1024,
                flushers: 2,
                buffer_pool_bytes: 16 * 1024 * 1024,
                direct_io: false,
            },
        )
        .await
        .unwrap();
        let index = Arc::new(ObjectIndex::open(&scratch.path().join("index.redb")).unwrap());

        // The origin is on loopback, so the guard has to be told this is deliberate. In
        // production allow_private is off and 127.0.0.1 is refused.
        let resolver =
            Arc::new(UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], true).unwrap());
        let upstream = UpstreamClient::new(resolver, ClientConfig::default()).unwrap();
        let orchestrator = Arc::new(Orchestrator::new(store, index, upstream, SLICE, 8));

        // A single service matching the mock origin's host.
        let host = origin.addr().ip().to_string();
        let index_json = r#"{"cache_domains":[{"name":"mock","domain_files":["m.txt"]}]}"#;
        let mut files = std::collections::BTreeMap::new();
        files.insert("m.txt".to_string(), format!("{host}\n"));
        let matcher = Arc::new(Matcher::build(
            &DomainList::parse(index_json, &files).unwrap(),
        ));

        let config = Arc::new(ServerConfig {
            orchestrator: orchestrator.clone(),
            matcher,
            rules: Arc::new(Rules::default()),
            compiled: Arc::new(HashMap::new()),
            hostname: "test-cache".into(),
            passthrough_unknown_hosts: false,
        });
        let server = Server::bind("127.0.0.1:0".parse().unwrap(), config)
            .await
            .unwrap();

        Self {
            _scratch: scratch,
            origin,
            server,
            orchestrator,
        }
    }

    /// The mock origin's host, which is also the Host header clients must send.
    fn origin_host(&self) -> String {
        self.origin.addr().to_string()
    }

    fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap()
    }

    /// Request a path through the proxy, with the Host header the matcher expects.
    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        self.client()
            .get(format!("{}{}", self.server.base_url(), path))
            .header("host", self.origin_host())
    }
}

/// Deterministic RNG so a failure reproduces from the printed seed.
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

#[tokio::test]
async fn serves_a_full_object_matching_the_origin() {
    let h = Harness::start("m1-full", CdnConfig::default()).await;
    let size = 200_000u64;
    let path = MockCdn::object_path("full", size);

    let response = h.request(&path).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-cache"], "MISS");
    assert_eq!(response.headers()["x-lancache-processed-by"], "test-cache");
    let body = response.bytes().await.unwrap();
    assert_eq!(
        body.as_ref(),
        content::range(content::seed_for("full"), 0, size as usize).as_slice()
    );
}

#[tokio::test]
async fn random_ranges_match_the_origin_cold_and_warm() {
    let h = Harness::start("m1-diff", CdnConfig::default()).await;
    let seed = 0xD1FF_0000_1234_5678u64;
    let mut rng = Rng(seed);
    let size = 5 * SLICE as u64 + 4_321;

    for pass in 0..2 {
        for iteration in 0..30 {
            let name = format!("obj-{}", rng.below(5));
            let path = MockCdn::object_path(&name, size);
            let start = rng.below(size);
            let len = 1 + rng.below(size - start);
            let end = start + len - 1;

            let response = h
                .request(&path)
                .header("range", format!("bytes={start}-{end}"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                206,
                "seed {seed:#x} pass {pass} iteration {iteration}"
            );
            let body = response.bytes().await.unwrap();
            assert_eq!(
                body.as_ref(),
                content::range(content::seed_for(&name), start, len as usize).as_slice(),
                "seed {seed:#x} pass {pass} iteration {iteration}: {start}-{end} of {name}"
            );
        }
    }
}

#[tokio::test]
async fn a_warm_read_does_not_touch_the_origin() {
    let h = Harness::start("m1-warm", CdnConfig::default()).await;
    let size = 3 * SLICE as u64;
    let path = MockCdn::object_path("warm", size);

    let first = h.request(&path).send().await.unwrap();
    assert_eq!(first.headers()["x-cache"], "MISS");
    let first_body = first.bytes().await.unwrap();
    let after_first = h.origin.stats().requests();

    let second = h.request(&path).send().await.unwrap();
    assert_eq!(second.headers()["x-cache"], "HIT");
    assert_eq!(second.bytes().await.unwrap(), first_body);
    assert_eq!(h.origin.stats().requests(), after_first);
}

#[tokio::test]
async fn concurrent_clients_share_one_fetch_per_slice() {
    // FR-30 end to end. This is the behaviour bought over nginx's proxy_cache_lock.
    let h = Harness::start(
        "m1-coalesce",
        CdnConfig {
            first_byte_delay: Some(Duration::from_millis(60)),
            ..CdnConfig::default()
        },
    )
    .await;
    let size = 8 * SLICE as u64;
    let path = MockCdn::object_path("hot", size);
    let slices = size / SLICE as u64;

    let mut handles = Vec::new();
    for _ in 0..24 {
        let client = h.client();
        let url = format!("{}{}", h.server.base_url(), path);
        let host = h.origin_host();
        handles.push(tokio::spawn(async move {
            let r = client.get(url).header("host", host).send().await.unwrap();
            assert_eq!(r.status(), 200);
            r.bytes().await.unwrap()
        }));
    }
    let expected = content::range(content::seed_for("hot"), 0, size as usize);
    for handle in handles {
        assert_eq!(handle.await.unwrap().as_ref(), expected.as_slice());
    }

    let upstream = h.origin.stats().requests();
    assert!(
        upstream <= slices + 4,
        "24 clients caused {upstream} upstream requests for {slices} slices"
    );
}

#[tokio::test]
async fn a_client_disconnect_does_not_cancel_the_fill() {
    // FR-31. The M0 spike got this wrong: awaiting slice futures inline cancels them when the
    // response stream is dropped, throwing away work a later client would have used.
    let h = Harness::start(
        "m1-disconnect",
        CdnConfig {
            // Slow enough that the client can hang up mid-body.
            chunk_delay: Some(Duration::from_millis(40)),
            ..CdnConfig::default()
        },
    )
    .await;
    let size = 4 * SLICE as u64;
    let path = MockCdn::object_path("abandoned", size);

    {
        let response = h.request(&path).send().await.unwrap();
        assert_eq!(response.status(), 200);
        // Read one chunk, then drop the response, which is what a client hanging up looks like.
        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;
        let _ = stream.next().await;
    }

    // Give the detached fills a moment to land.
    tokio::time::sleep(Duration::from_millis(600)).await;

    let key =
        cachic::services::key::normalise("mock", &h.origin_host(), &path, &CompiledRule::default());
    let object = key.object_id();
    // Slice 0 is stored by the probe, synchronously, before any pipeline work. Counting it would
    // make this test pass even if every fill were cancelled, so only slices the probe did not
    // fetch are evidence.
    let beyond_probe = (1..4)
        .filter(|i| {
            h.orchestrator
                .store()
                .contains(&cachic::store::slice::SliceKey::new(object, 0, *i))
        })
        .count();
    assert!(
        beyond_probe > 0,
        "the disconnect abandoned every slice past the probe; \
         fills must outlive their connection (FR-31)"
    );
}

#[tokio::test]
async fn an_unsatisfiable_range_is_416() {
    let h = Harness::start("m1-416", CdnConfig::default()).await;
    let size = 10_000u64;
    let path = MockCdn::object_path("small", size);
    let r = h
        .request(&path)
        .header("range", "bytes=50000-60000")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 416);
    assert_eq!(r.headers()["content-range"], format!("bytes */{size}"));
}

#[tokio::test]
async fn an_unmatched_host_is_404_not_a_proxy() {
    // Proxying unknown hosts would make this an open proxy on the LAN (FR-64).
    let h = Harness::start("m1-unmatched", CdnConfig::default()).await;
    let r = h
        .client()
        .get(format!("{}/anything", h.server.base_url()))
        .header("host", "not-a-cdn.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn head_returns_metadata_without_a_body() {
    let h = Harness::start("m1-head", CdnConfig::default()).await;
    let size = 123_456u64;
    let path = MockCdn::object_path("headable", size);
    let r = h
        .client()
        .head(format!("{}{}", h.server.base_url(), path))
        .header("host", h.origin_host())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-length"], size.to_string());
    assert!(r.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn the_heartbeat_answers_without_a_host_match() {
    // Prefill tools probe this before they know anything about services.
    let h = Harness::start("m1-heartbeat", CdnConfig::default()).await;
    let r = h
        .client()
        .get(format!("{}/lancache-heartbeat", h.server.base_url()))
        .header("host", "anything.example.com")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert_eq!(r.headers()["x-lancache-processed-by"], "test-cache");
}

#[tokio::test]
async fn ranges_spanning_the_short_final_slice_are_correct() {
    let h = Harness::start("m1-ragged", CdnConfig::default()).await;
    let size = 2 * SLICE as u64 + 7;
    let path = MockCdn::object_path("ragged", size);
    let seed = content::seed_for("ragged");

    for (start, end) in [
        (size - 1, size - 1),
        (size - 8, size - 1),
        (2 * SLICE as u64 - 1, size - 1),
        (0, size - 1),
    ] {
        let r = h
            .request(&path)
            .header("range", format!("bytes={start}-{end}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 206, "range {start}-{end}");
        assert_eq!(
            r.bytes().await.unwrap().as_ref(),
            content::range(seed, start, (end - start + 1) as usize).as_slice(),
            "range {start}-{end}"
        );
    }
}
