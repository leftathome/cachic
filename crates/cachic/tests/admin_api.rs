//! End-to-end tests for the admin API (TASK-19).
//!
//! These go over HTTP rather than calling the handlers directly, because the properties that
//! matter - that a purge actually removes slices, that auth rejects before doing any work - are
//! properties of the served surface, not of the functions.

use std::sync::Arc;

use bytes::Bytes;
use cachic::{
    admin::{
        api::{ApiState, AuthToken, ServiceInfo},
        AdminServer, AdminState, Readiness,
    },
    proxy::shutdown::Drain,
    store::{
        hybrid::{SliceStore, StoreConfig},
        index::{now_secs, ObjectIndex, ObjectMeta},
        slice::{object_id, SliceHeader, SliceKey, SliceValue},
    },
    telemetry::metrics::Metrics,
    test_support::Scratch,
};

const SLICE: u32 = 1024;

/// Parse a response body as JSON.
///
/// reqwest's `json()` needs a feature the production build deliberately does not carry, and a
/// test convenience is not a reason to widen what ships.
async fn json(response: reqwest::Response) -> serde_json::Value {
    let text = response.text().await.unwrap();
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("not JSON: {e}\n{text}"))
}

struct Harness {
    _scratch: Scratch,
    server: AdminServer,
    store: SliceStore,
    index: Arc<ObjectIndex>,
}

impl Harness {
    async fn start(tag: &str, token: AuthToken) -> Self {
        let scratch = Scratch::new(tag);
        let store = SliceStore::open(
            &scratch.path().join("slices"),
            &StoreConfig {
                memory_bytes: 8 * 1024 * 1024,
                disk_bytes: 64 * 1024 * 1024,
                block_bytes: 4 * 1024 * 1024,
                flushers: 2,
                buffer_pool_bytes: 8 * 1024 * 1024,
                direct_io: false,
            },
        )
        .await
        .unwrap();
        let index = Arc::new(ObjectIndex::open(&scratch.path().join("index.redb")).unwrap());
        let (metrics, _) = Metrics::new().unwrap();
        let readiness = Arc::new(Readiness::new());
        readiness.set_store_open(true);
        readiness.set_listeners_bound(true);

        let server = AdminServer::bind_with_api(
            "127.0.0.1:0".parse().unwrap(),
            AdminState {
                metrics: Arc::new(metrics),
                readiness: readiness.clone(),
            },
            {
                let late = cachic::admin::api::LateApiState::new();
                late.set(ApiState {
                    store: store.clone(),
                    index: index.clone(),
                    drain: Drain::new(),
                    readiness,
                    token,
                    services: Arc::new(vec![ServiceInfo {
                        name: "steam".into(),
                        patterns: 1,
                    }]),
                    data_dir: scratch.path().to_path_buf(),
                    configured_disk_bytes: 64 * 1024 * 1024,
                    min_free_bytes: 1024 * 1024,
                    slice_size: SLICE,
                    // Loopback in tests, so the destructive endpoints rely on the address.
                    mutations_need_token: false,
                });
                late
            },
        )
        .await
        .unwrap();

        Self {
            _scratch: scratch,
            server,
            store,
            index,
        }
    }

    /// Store an object of `slices` slices with the given key.
    async fn seed(&self, key: &str, slices: u32) {
        let id = object_id(key);
        let total_len = (slices as u64) * SLICE as u64;
        let now = now_secs();
        self.index
            .put(
                &id,
                &ObjectMeta {
                    key: key.into(),
                    total_len,
                    generation: 0,
                    etag: Some("\"seed\"".into()),
                    last_modified: None,
                    content_type: None,
                    no_ranges: false,
                    created: now,
                    last_seen: now,
                    stale: false,
                },
            )
            .unwrap();
        for i in 0..slices {
            self.store.insert(
                SliceKey::new(id, 0, i),
                SliceValue::new(
                    SliceHeader {
                        slice_size: SLICE,
                        total_len,
                        generation: 0,
                        etag: Some("\"seed\"".into()),
                        last_modified: None,
                        content_type: None,
                    },
                    Bytes::from(vec![0u8; SLICE as usize]),
                ),
            );
        }
    }

    fn resident(&self, key: &str, slices: u32) -> usize {
        let id = object_id(key);
        (0..slices)
            .filter(|i| self.store.contains(&SliceKey::new(id, 0, *i)))
            .count()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.server.base_url(), path)
    }
}

#[tokio::test]
async fn stats_report_the_cache_and_the_disk_guard() {
    let h = Harness::start("admin-stats", AuthToken::none()).await;
    h.seed("/depot/440/a", 4).await;

    let stats = json(reqwest::get(h.url("/stats")).await.unwrap()).await;

    assert_eq!(stats["objects"], 1);
    assert_eq!(stats["indexed_bytes"], 4 * SLICE as u64);
    assert_eq!(stats["slice_size"], SLICE);
    // The guard's inputs must be visible, since "why is my cache not growing" is the question it
    // exists to answer.
    assert!(stats["filesystem_available_bytes"].as_u64().unwrap() > 0);
    assert!(stats.get("disk_guard_engaged").is_some());
    assert!(stats.get("effective_disk_bytes").is_some());
}

#[tokio::test]
async fn purge_by_prefix_removes_slices_and_index_entries_together() {
    // Leaving slices behind would waste the space the operator was reclaiming; leaving index
    // entries behind would let a later request believe the object is still known.
    let h = Harness::start("admin-purge", AuthToken::none()).await;
    h.seed("/depot/440/a", 4).await;
    h.seed("/depot/440/b", 2).await;
    h.seed("/depot/570/c", 3).await;
    assert_eq!(h.resident("/depot/440/a", 4), 4);

    let result = json(
        reqwest::Client::new()
            .post(h.url("/purge?prefix=/depot/440/"))
            .send()
            .await
            .unwrap(),
    )
    .await;

    assert_eq!(result["objects_removed"], 2);
    assert_eq!(result["slices_removed"], 6);
    assert_eq!(
        h.resident("/depot/440/a", 4),
        0,
        "slices survived the purge"
    );
    assert_eq!(h.resident("/depot/440/b", 2), 0);
    assert!(h.index.get(&object_id("/depot/440/a")).unwrap().is_none());
    // The untouched service is untouched.
    assert_eq!(h.resident("/depot/570/c", 3), 3);
    assert!(h.index.get(&object_id("/depot/570/c")).unwrap().is_some());
}

#[tokio::test]
async fn purge_without_a_prefix_or_all_is_refused() {
    // A request that meant to carry a prefix and lost it must not empty the cache.
    let h = Harness::start("admin-purge-guard", AuthToken::none()).await;
    h.seed("/a", 1).await;

    let response = reqwest::Client::new()
        .post(h.url("/purge"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert_eq!(
        h.resident("/a", 1),
        1,
        "the cache was purged by an ambiguous request"
    );
}

#[tokio::test]
async fn purge_all_requires_saying_so() {
    let h = Harness::start("admin-purge-all", AuthToken::none()).await;
    h.seed("/a", 2).await;
    h.seed("/b", 2).await;

    let result = json(
        reqwest::Client::new()
            .post(h.url("/purge?all=true"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["objects_removed"], 2);
    assert_eq!(h.resident("/a", 2), 0);
    assert_eq!(h.resident("/b", 2), 0);
}

#[tokio::test]
async fn every_endpoint_requires_the_token_when_one_is_configured() {
    let h = Harness::start("admin-auth", AuthToken::new("s3cret")).await;
    h.seed("/a", 1).await;
    let client = reqwest::Client::new();

    for path in ["/stats", "/services"] {
        assert_eq!(
            client.get(h.url(path)).send().await.unwrap().status(),
            401,
            "{path} served without a token"
        );
    }
    for path in ["/purge?all=true", "/drain"] {
        assert_eq!(
            client.post(h.url(path)).send().await.unwrap().status(),
            401,
            "{path} served without a token"
        );
    }

    // And the unauthorised purge did nothing.
    assert_eq!(h.resident("/a", 1), 1, "an unauthorised purge still purged");

    // With the token, the same requests work.
    assert_eq!(
        client
            .get(h.url("/stats"))
            .bearer_auth("s3cret")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn the_probes_stay_unauthenticated() {
    // An orchestrator's liveness probe cannot carry a bearer token, so requiring one would make
    // the pod permanently unhealthy.
    let h = Harness::start("admin-probes", AuthToken::new("s3cret")).await;
    for path in ["/healthz", "/readyz"] {
        assert_eq!(
            reqwest::get(h.url(path)).await.unwrap().status(),
            200,
            "{path} required authentication"
        );
    }
}

#[tokio::test]
async fn drain_fails_readiness_immediately() {
    let h = Harness::start("admin-drain", AuthToken::none()).await;
    assert_eq!(reqwest::get(h.url("/readyz")).await.unwrap().status(), 200);

    let response = reqwest::Client::new()
        .post(h.url("/drain"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);

    assert_eq!(
        reqwest::get(h.url("/readyz")).await.unwrap().status(),
        503,
        "readiness did not fail after drain was requested"
    );
}

#[tokio::test]
async fn services_are_listed() {
    let h = Harness::start("admin-services", AuthToken::none()).await;
    let services = json(reqwest::get(h.url("/services")).await.unwrap()).await;
    assert_eq!(services[0]["name"], "steam");
}
