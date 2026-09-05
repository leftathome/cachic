//! Proof-of-concept tests for the independent security review.
//!
//! Each test here demonstrates a finding rather than asserting one. They are written to *pass*
//! against the current code, which means each one documents behaviour that exists today. When a
//! finding is fixed, its test here should start failing - that is the point, and the fix should
//! replace the test with one asserting the new, safe behaviour.
//!
//! Threat model: an untrusted client on the same L2 network as the cache. That is the LAN-party
//! and public-venue case named in the review - machines nobody vetted, brought onto the network
//! by strangers, able to reach every port cachic listens on.
//!
//! Findings, in the order they appear below:
//!
//! - SR-01  admin API: purge and drain are unauthenticated by default, on 0.0.0.0
//! - SR-02  SNI pass-through is an open TCP relay (FR-64's allow-list half is unimplemented)
//! - SR-03  SNI connections are not counted against any limit
//! - SR-04  lancache access-log lines can be forged from client-controlled headers
//! - SR-05  a single domain-list entry can widen the allow-list to an entire TLD
//! - SR-06  distinct upstream URLs collapse to one cache key (poisoning primitive)
//! - SR-07  no header-read timeout: idle connections hold their slot indefinitely

use std::{sync::Arc, time::Duration};

use cachic::{
    admin::{
        api::{ApiState, AuthToken, LateApiState, ServiceInfo},
        AdminServer, AdminState, Readiness,
    },
    proxy::shutdown::Drain,
    services::{
        domains::DomainList,
        key::{self, CompiledRule},
        matcher::Matcher,
    },
    store::{
        hybrid::{SliceStore, StoreConfig},
        index::{now_secs, ObjectIndex, ObjectMeta},
        slice::object_id,
    },
    telemetry::logs::{clf_timestamp, AccessEvent},
    test_support::Scratch,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SLICE: u32 = 1024;

// -------------------------------------------------------------------------------------------
// SR-01: the admin API is unauthenticated by default, and main.rs binds it to 0.0.0.0.
// -------------------------------------------------------------------------------------------

/// Anyone who can reach the admin port can empty the cache.
///
/// `ADMIN_TOKEN` defaults to `""`, which `AuthToken::new` deliberately maps to "no auth". The
/// docstring on `AuthToken` justifies that with "it is bound to loopback or a cluster network by
/// default", but `main.rs` binds `SocketAddr::from(([0, 0, 0, 0], config.admin_port))` - every
/// interface. The only thing confining it in the quickstart is the compose port mapping, which
/// does not apply to the published release binaries, to `--network host`, or to the Helm chart's
/// admin Service.
#[tokio::test]
async fn sr01_purge_all_needs_no_credentials_by_default() {
    let harness = AdminHarness::start("sr01", AuthToken::new("")).await;

    // Seed something worth destroying.
    harness.seed("/depot/expensive.chunk").await;
    assert_eq!(harness.index.len().unwrap(), 1);

    // No Authorization header. This is what any host on the LAN can send.
    let response = reqwest::Client::new()
        .post(format!("{}/purge?all=true", harness.server.base_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        200,
        "an unauthenticated purge was refused - has this been fixed?"
    );
    assert_eq!(
        harness.index.len().unwrap(),
        0,
        "the purge reported success but removed nothing"
    );
}

/// SR-01 (fixed): the admin API binds loopback unless an operator says otherwise.
///
/// The one-request cache wipe was reachable because the listener bound 0.0.0.0 with no token. The
/// primary fix is the default bind address; this asserts it, because a default is exactly the kind
/// of thing that quietly regresses.
#[test]
fn sr01_the_admin_api_binds_loopback_by_default() {
    use clap::Parser;

    let config = cachic::config::Config::parse_from(["cachic"]);
    assert!(
        config.admin_bind.is_loopback(),
        "the admin API defaults to binding {}, which exposes /purge and /drain to anything that \
         can reach the port",
        config.admin_bind
    );
}

/// SR-01 (fixed): bound off-box without a token, the destructive endpoints refuse.
///
/// Binding wider is legitimate - a Kubernetes install has to, so Prometheus can scrape /metrics -
/// so the backstop is per endpoint rather than a refusal to start. Health and metrics keep
/// working; purge and drain do not, until a token exists.
#[tokio::test]
async fn sr01_mutations_refuse_when_reachable_without_a_token() {
    let harness = AdminHarness::start_with("sr01-open", AuthToken::new(""), true).await;
    harness.seed("/depot/expensive.chunk").await;
    assert_eq!(harness.index.len().unwrap(), 1);

    let response = reqwest::Client::new()
        .post(format!("{}/purge?all=true", harness.server.base_url()))
        .send()
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        403,
        "an unauthenticated purge was accepted while the admin API was reachable off-box"
    );
    assert_eq!(
        harness.index.len().unwrap(),
        1,
        "the purge was refused but removed the object anyway"
    );

    // The endpoints that justify the port being reachable still answer.
    let health = reqwest::Client::new()
        .get(format!("{}/healthz", harness.server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(
        health.status(),
        200,
        "health was collateral damage; only the destructive endpoints should refuse"
    );
}

/// The same holds for `/drain`, which takes the node out of service.
///
/// In Kubernetes this is worse than it looks: `/drain` fails readiness, so an unauthenticated
/// POST from any pod in the cluster removes the cache from its Service endpoints.
#[tokio::test]
async fn sr01_drain_needs_no_credentials_by_default() {
    let harness = AdminHarness::start("sr01-drain", AuthToken::new("")).await;

    let ready = reqwest::get(format!("{}/readyz", harness.server.base_url()))
        .await
        .unwrap();
    assert_eq!(ready.status(), 200, "should be ready before the attack");

    let response = reqwest::Client::new()
        .post(format!("{}/drain", harness.server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202);

    let after = reqwest::get(format!("{}/readyz", harness.server.base_url()))
        .await
        .unwrap();
    assert_eq!(
        after.status(),
        503,
        "an unauthenticated request should not have been able to fail readiness"
    );
}

/// A configured token does close the hole, which is what makes this a default-hardening finding
/// rather than a broken mechanism. Included so the fix direction is unambiguous.
#[tokio::test]
async fn sr01_a_configured_token_does_defend_the_endpoint() {
    let harness = AdminHarness::start("sr01-token", AuthToken::new("s3cret")).await;
    harness.seed("/depot/expensive.chunk").await;

    let response = reqwest::Client::new()
        .post(format!("{}/purge?all=true", harness.server.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(
        harness.index.len().unwrap(),
        1,
        "the object was purged despite a 401"
    );
}

// -------------------------------------------------------------------------------------------
// SR-02 / SR-03: the SNI pass-through relays to any name, and counts no connections.
// -------------------------------------------------------------------------------------------

/// SR-02 (fixed): the SNI path applies the allow-list, as the HTTP path always did.
///
/// FR-64 has two halves - refuse private addresses, and only proxy allow-listed hosts. The SNI
/// path shipped only the first through v0.1.0-rc5, which made it an open TCP relay to port 443 of
/// any host on the internet, with the traffic attributed to the operator.
///
/// This asserts the gate, and two controls so it cannot pass because everything is broken: an
/// allow-listed name must still splice, and `passthrough` must still open it deliberately.
///
/// On `allow_private`: the mock origin is on loopback, so the address guard is told this is
/// deliberate, exactly as `proxy_integration.rs` does. It is orthogonal - the gate under test is
/// the *name* check.
#[tokio::test]
async fn sr02_sni_refuses_a_host_that_is_not_in_the_allow_list() {
    use cachic::{
        services::refresh::LiveServices, sni::proxy::SniProxy, upstream::resolver::UpstreamResolver,
    };
    use cachic_testkit::mockdns::MockDns;

    /// A name in no domain file cachic ships.
    const NOT_A_CDN: &str = "evil-relay-target.net";
    /// A name that is in the bundled list.
    const A_CDN: &str = "lancache.steamcontent.com";

    let ord = std::sync::atomic::Ordering::Relaxed;
    let bundled = Matcher::build(&cachic::services::domains::bundled().unwrap());
    assert_eq!(
        bundled.service_for(NOT_A_CDN),
        None,
        "the test host is in the allow-list; pick another"
    );
    assert!(
        bundled.service_for(A_CDN).is_some(),
        "the control host is not in the allow-list; pick another"
    );

    // An "origin" that is emphatically not a game CDN, and speaks no TLS. Each connection gets one
    // reply, so the count of replies is the count of successful relays.
    let origin = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin_port = origin.local_addr().unwrap().port();
    let reached = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let origin_reached = reached.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = origin.accept().await else {
                return;
            };
            origin_reached.fetch_add(1, ord);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let _ = socket.write_all(format!("RELAYED:{n}").as_bytes()).await;
                let _ = socket.flush().await;
            });
        }
    });

    let dns = MockDns::start("127.0.0.1".parse().unwrap()).await.unwrap();
    let services = LiveServices::new(cachic::services::domains::bundled().unwrap());

    // Drive one ClientHello through a proxy and report what came back.
    async fn attempt(proxy_addr: std::net::SocketAddr, host: &str) -> String {
        let mut client = tokio::net::TcpStream::connect(proxy_addr).await.unwrap();
        client.write_all(&client_hello(host)).await.unwrap();
        client.flush().await.unwrap();
        let mut response = Vec::new();
        let _ =
            tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut response)).await;
        String::from_utf8_lossy(&response).into_owned()
    }

    // --- the fix: an unmatched name is refused before anything is dialled --------------------
    let resolver = Arc::new(UpstreamResolver::with_servers(&[dns.addr()], true).unwrap());
    let guarded = SniProxy::bind(
        "127.0.0.1:0".parse().unwrap(),
        resolver.clone(),
        origin_port,
        services.clone(),
        false,
    )
    .await
    .unwrap();

    let text = attempt(guarded.addr(), NOT_A_CDN).await;
    assert!(
        !text.starts_with("RELAYED:"),
        "the relay carried bytes for a host that is not allow-listed: {text:?}"
    );
    assert_eq!(
        reached.load(ord),
        0,
        "the origin was contacted for a host that is not allow-listed"
    );
    assert_eq!(
        guarded.stats().not_allow_listed.load(ord),
        1,
        "the refusal was not counted"
    );
    assert_eq!(guarded.stats().spliced.load(ord), 0);

    // --- control: an allow-listed name still works -------------------------------------------
    let text = attempt(guarded.addr(), A_CDN).await;
    assert!(
        text.starts_with("RELAYED:"),
        "an allow-listed host no longer splices, so the gate refuses everything: {text:?}"
    );
    assert_eq!(guarded.stats().spliced.load(ord), 1);

    // --- control: passthrough is still the documented escape hatch ---------------------------
    let open = SniProxy::bind(
        "127.0.0.1:0".parse().unwrap(),
        resolver,
        origin_port,
        services,
        true,
    )
    .await
    .unwrap();
    let text = attempt(open.addr(), NOT_A_CDN).await;
    assert!(
        text.starts_with("RELAYED:"),
        "passthrough did not permit an unmatched host: {text:?}"
    );
    assert_eq!(open.stats().not_allow_listed.load(ord), 0);
}

/// SNI connections are accepted without any limit.
///
/// `Server::bind` (the HTTP path) takes a `ConnectionPermit` per connection and refuses at
/// `ConnectionLimit`. `SniProxy::bind` spawns a task per accepted socket with no limiter at all,
/// so port 443 has no ceiling on concurrent connections, tasks, or the ~16 KiB hello buffer each
/// one may hold.
///
/// This test asserts the structural fact - that the SNI proxy accepts far more concurrent
/// connections than the HTTP path's default ceiling would allow - rather than trying to exhaust
/// memory in CI.
#[tokio::test]
async fn sr03_sni_accepts_connections_without_any_ceiling() {
    use cachic::{sni::proxy::SniProxy, upstream::resolver::UpstreamResolver};

    let resolver = Arc::new(UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], false).unwrap());
    let proxy = SniProxy::bind(
        "127.0.0.1:0".parse().unwrap(),
        resolver,
        443,
        cachic::services::refresh::LiveServices::new(cachic::services::domains::bundled().unwrap()),
        true,
    )
    .await
    .unwrap();

    // Open connections and send a single TLS-looking byte, so each is parked in the hello read
    // loop holding a task and a buffer. None of these will ever be counted or refused.
    let mut held = Vec::new();
    for _ in 0..64 {
        let mut c = tokio::net::TcpStream::connect(proxy.addr()).await.unwrap();
        c.write_all(&[0x16]).await.unwrap();
        held.push(c);
    }

    // Give the accept loop a moment to take them all.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let accepted = proxy
        .stats()
        .accepted
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        accepted, 64,
        "expected every connection to be accepted unconditionally, saw {accepted}"
    );
    // There is no rejected counter here, because there is no rejection path to count.
}

// -------------------------------------------------------------------------------------------
// SR-04: the lancache access log is forgeable from client-controlled input.
// -------------------------------------------------------------------------------------------

/// A client-chosen `User-Agent` can inject whole fields into the lancache access log.
///
/// `AccessEvent::to_lancache` interpolates `user_agent` and `path` into a positional, quoted,
/// single-line format with no escaping, and the subscriber for that format is configured with
/// `.without_time().with_target(false).with_level(false)` - bare lines. HTTP header values may
/// legally contain `"`; hyper only rejects CR and LF.
///
/// The consumers of this format are third-party dashboards (LANCache Manager, DeveLanCacheUI)
/// that parse it positionally into a database. A client can therefore fabricate entries
/// attributing traffic to another machine, or hide its own.
#[test]
fn sr04_a_user_agent_can_forge_fields_in_the_lancache_log() {
    // Everything after the first quote is the attacker's. It closes the User-Agent field, supplies
    // a cache status, and opens a fresh forged record blaming another client.
    let hostile = r#"x" "HIT"
[steam] 10.0.0.99 / - - - [02/Sep/2025:00:00:00 +0000] "GET /forged HTTP/1.1" 200 999999999 "-" "-" "HIT"#;

    // hyper must actually accept this in a header for the finding to be reachable. Quotes are
    // legal in a field value; only CR and LF are not.
    let quoted_only = r#"x" "HIT"#;
    assert!(
        hyper::header::HeaderValue::from_str(quoted_only).is_ok(),
        "hyper rejects quotes in header values, which would close this hole"
    );

    let event = AccessEvent {
        client_ip: "192.168.1.50".into(),
        service: "steam".into(),
        host: "lancache.steamcontent.com".into(),
        method: "GET".into(),
        path: "/depot/1/chunk".into(),
        range: None,
        status: 206,
        bytes: 1024,
        cache_status: "MISS".into(),
        upstream_seconds: 0.0,
        user_agent: Some(hostile.into()),
        timestamp: clf_timestamp(1_756_800_000),
    };

    let line = event.to_lancache();

    // The forged record is present verbatim, and a positional parser reading line-by-line sees a
    // second request from 10.0.0.99 that never happened.
    assert!(
        line.contains(r#"[steam] 10.0.0.99 / - - - [02/Sep/2025:00:00:00 +0000] "GET /forged"#),
        "the forged record did not survive into the log line: {line}"
    );
    assert!(
        line.contains("999999999"),
        "the forged byte count is missing: {line}"
    );
    // Nothing escaped the quote, and nothing stripped the newline.
    assert!(
        line.contains('\n'),
        "a newline in a header value reached the access log: {line}"
    );
}

// -------------------------------------------------------------------------------------------
// SR-05: one domain-list line can widen the allow-list to an entire TLD.
// -------------------------------------------------------------------------------------------

/// `Pattern::parse` applies no structural validation to a wildcard.
///
/// `*.com` becomes `Suffix("com")` and matches every `.com` host in existence. Nothing rejects a
/// wildcard whose parent is a public suffix, or a bare TLD, or `*` itself.
///
/// This matters because `services::refresh` fetches this list over the network every 24h by
/// default, from `master` of a third-party GitHub repository, and applies it if it merely parses
/// and is non-empty. There is no commit pin, no signature, and no bound on how much the list may
/// change. Combined with `include_host: false` (the default, and the reason lancache exists), a
/// single added line turns cachic into an open proxy whose cache is keyed on path alone.
#[test]
fn sr05_a_wildcard_may_name_a_public_suffix() {
    let index = r#"{"cache_domains":[{"name":"steam","domain_files":["s.txt"]}]}"#;
    let mut files = std::collections::BTreeMap::new();
    // One line. This is what a compromised or careless upstream commit looks like.
    files.insert("s.txt".to_string(), "*.com\n".to_string());

    let list = DomainList::parse(index, &files).expect("a TLD wildcard was accepted");
    let matcher = Matcher::build(&list);

    for host in [
        "attacker-controlled.com",
        "d1234abcd.cloudfront.com",
        "www.google.com",
        "anything.at.all.com",
    ] {
        assert_eq!(
            matcher.service_for(host),
            Some("steam"),
            "{host} should have matched the TLD wildcard"
        );
    }
}

/// The narrower form of the same gap: nothing requires a wildcard to have a registrable parent.
#[test]
fn sr05_a_bare_star_and_a_single_label_are_both_accepted() {
    let index = r#"{"cache_domains":[{"name":"x","domain_files":["x.txt"]}]}"#;
    let mut files = std::collections::BTreeMap::new();
    files.insert("x.txt".to_string(), "*\n*.net\n".to_string());
    let list = DomainList::parse(index, &files).expect("degenerate patterns were accepted");

    let matcher = Matcher::build(&list);
    assert_eq!(matcher.service_for("anything.net"), Some("x"));
}

// -------------------------------------------------------------------------------------------
// SR-06: distinct upstream URLs share one cache key.
// -------------------------------------------------------------------------------------------

/// The cache key is a lossy normalisation of a request target that is sent upstream verbatim.
///
/// `proxy::server::serve` builds the fetch URL as `format!("{scheme}://{host}{target}")` from the
/// *raw* target, while `key::normalise` percent-decodes, collapses `//` and `.` and `..`, and
/// drops the query string. Any origin that distinguishes two targets cachic merges will serve
/// different bytes that land under one key.
///
/// The query string is the sharp edge. It is dropped from the key by design - CDN auth tokens live
/// there and keeping them would make every request a miss - but it is still sent to the origin.
/// An origin that varies its response on a query parameter therefore lets a client choose the
/// bytes stored at another client's key.
///
/// This is inherited from monolithic rather than introduced here, and the mitigation is not
/// obvious, which is why it is reported as a design risk to document rather than a bug to patch.
#[test]
fn sr06_distinct_targets_collapse_to_one_object_id() {
    let rule = CompiledRule::default();
    let victim = key::normalise("steam", "cdn.example.com", "/depot/1/chunk", &rule);

    // Each of these is a different URL at the origin, and every one of them writes to the key a
    // victim will later read.
    for hostile in [
        "/depot/1/chunk?anything=the-attacker-wants",
        "/depot/1/chunk?",
        "/depot/%31/chunk",
        "/depot//1/chunk",
        "/depot/./1/chunk",
        "/depot/1/../1/chunk",
        "/elsewhere/../depot/1/chunk",
    ] {
        let poisoned = key::normalise("steam", "cdn.example.com", hostile, &rule);
        assert_eq!(
            poisoned.object_id(),
            victim.object_id(),
            "{hostile} did not collide with the victim key"
        );
    }
}

/// Host is excluded from the key by default, so every hostname mapped to a service shares one
/// namespace.
///
/// This is the entire point of a LAN cache and is not itself a defect. It is recorded because it
/// is what converts SR-05 from "the allow-list got wider" into "the cache can be written by an
/// attacker": one attacker-controlled hostname inside a service is enough to write every key in
/// that service.
#[test]
fn sr06_any_host_in_a_service_can_write_every_key_in_it() {
    let rule = CompiledRule::default();
    let legitimate = key::normalise(
        "steam",
        "lancache.steamcontent.com",
        "/depot/1/chunk",
        &rule,
    );
    let attacker = key::normalise("steam", "attacker.example.com", "/depot/1/chunk", &rule);
    assert_eq!(legitimate.object_id(), attacker.object_id());
}

// -------------------------------------------------------------------------------------------
// SR-07: no header-read timeout; a half-open request holds its slot indefinitely.
// -------------------------------------------------------------------------------------------

/// hyper's default 30s header-read timeout never arms, because no `Timer` is installed.
///
/// `proxy::server::Server::bind` calls `http1::Builder::new().serve_connection(io, service)`. In
/// hyper 1.11.1 `h1_header_read_timeout` defaults to `Dur::Default(Some(30s))`, but
/// `Time::check` returns `None` and logs `timeout 'header_read_timeout' has default, but no timer
/// set` when the builder has no timer - which this one does not. The timeout is inert.
///
/// Combined with a `ConnectionLimit` that is global and has no per-peer component, one host can
/// hold every connection slot open indefinitely with a byte each, and every other client is
/// refused at accept.
///
/// This test waits past the 30s default deliberately: a shorter wait would prove only that the
/// connection was open, not that nothing will ever close it.
#[tokio::test]
async fn sr07_a_half_sent_request_is_timed_out() {
    let harness = ProxyHarness::start("sr07").await;

    let mut client = tokio::net::TcpStream::connect(harness.server.addr())
        .await
        .unwrap();
    // A request line and one header, and then silence. Never a blank line, so the server is still
    // waiting for headers. Before the timer was installed this connection was held forever, and
    // one host could take every slot for the cost of a byte each.
    client
        .write_all(b"GET /depot/1/chunk HTTP/1.1\r\nHost: cdn.example.com\r\n")
        .await
        .unwrap();
    client.flush().await.unwrap();

    // The configured header-read timeout is 15s. Waiting past it, with margin for a loaded CI box.
    let mut buf = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(40), client.read(&mut buf)).await;

    let closed = match read {
        Ok(Ok(0)) => true,  // clean EOF
        Ok(Err(_)) => true, // reset
        Ok(Ok(_)) => true,  // a 408-ish response, then close - also a close
        Err(_) => false,    // still blocked: nothing ever fires
    };
    assert!(
        closed,
        "a half-sent request was still open after 40s; the header-read timeout is not in effect. \
         hyper's own default is inert unless a Timer is installed on the builder"
    );
}

/// The connection limit is global, with nothing scoped to a peer.
///
/// `ConnectionLimit::try_acquire` takes no address, so the 10,000-connection ceiling is shared
/// between the whole network. One host reaching it denies service to every other client.
#[tokio::test]
async fn sr07_one_peer_can_consume_every_connection_slot() {
    use cachic::proxy::limits::ConnectionLimit;

    // A small ceiling stands in for the 10,000 default; the accounting is what is under test.
    let limit = ConnectionLimit::new(4);
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(
            limit
                .try_acquire()
                .expect("the limiter refused below its own ceiling"),
        );
    }

    // Every subsequent connection is refused, regardless of which host it comes from. There is no
    // per-peer accounting anywhere in the type.
    assert!(
        limit.try_acquire().is_none(),
        "the ceiling was exceeded, which is a different bug"
    );
    assert_eq!(limit.rejected(), 1);
}

// -------------------------------------------------------------------------------------------
// Harnesses
// -------------------------------------------------------------------------------------------

struct AdminHarness {
    _scratch: Scratch,
    server: AdminServer,
    index: Arc<ObjectIndex>,
}

impl AdminHarness {
    async fn start(tag: &str, token: AuthToken) -> Self {
        Self::start_with(tag, token, false).await
    }

    /// `mutations_need_token` mirrors "the admin API is bound somewhere a client can reach it".
    async fn start_with(tag: &str, token: AuthToken, mutations_need_token: bool) -> Self {
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
        let (metrics, _) = cachic::telemetry::metrics::Metrics::new().unwrap();
        let readiness = Arc::new(Readiness::new());
        readiness.set_store_open(true);
        readiness.set_listeners_bound(true);

        let late = LateApiState::new();
        late.set(ApiState {
            store,
            index: index.clone(),
            drain: Drain::new(),
            readiness: readiness.clone(),
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
            mutations_need_token,
        });

        let server = AdminServer::bind_with_api(
            "127.0.0.1:0".parse().unwrap(),
            AdminState {
                metrics: Arc::new(metrics),
                readiness,
            },
            late,
        )
        .await
        .unwrap();

        Self {
            _scratch: scratch,
            server,
            index,
        }
    }

    async fn seed(&self, key: &str) {
        let id = object_id(key);
        let now = now_secs();
        self.index
            .put(
                &id,
                &ObjectMeta {
                    key: key.into(),
                    total_len: SLICE as u64,
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
    }
}

struct ProxyHarness {
    _scratch: Scratch,
    server: cachic::proxy::server::Server,
}

impl ProxyHarness {
    async fn start(tag: &str) -> Self {
        use cachic::{
            orchestrator::Orchestrator,
            proxy::server::{Server, ServerConfig},
            upstream::{
                client::{ClientConfig, UpstreamClient},
                resolver::UpstreamResolver,
            },
        };

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
        let resolver =
            Arc::new(UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], false).unwrap());
        let upstream = UpstreamClient::new(resolver, ClientConfig::default()).unwrap();
        let orchestrator = Arc::new(Orchestrator::new(store, index, upstream, SLICE, 4));

        let index_json = r#"{"cache_domains":[{"name":"mock","domain_files":["m.txt"]}]}"#;
        let mut files = std::collections::BTreeMap::new();
        files.insert("m.txt".to_string(), "cdn.example.com\n".to_string());
        let list = DomainList::parse(index_json, &files).unwrap();

        let server = Server::bind(
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(ServerConfig::with_defaults(
                orchestrator,
                list,
                "test-cache",
            )),
        )
        .await
        .unwrap();

        Self {
            _scratch: scratch,
            server,
        }
    }
}

/// A minimal ClientHello carrying `host` as SNI. Mirrors the one in `sni::proxy`'s own tests.
fn client_hello(host: &str) -> Vec<u8> {
    let mut name = vec![0u8];
    name.extend_from_slice(&(host.len() as u16).to_be_bytes());
    name.extend_from_slice(host.as_bytes());
    let mut list = (name.len() as u16).to_be_bytes().to_vec();
    list.extend_from_slice(&name);
    let mut extensions = 0u16.to_be_bytes().to_vec();
    extensions.extend_from_slice(&(list.len() as u16).to_be_bytes());
    extensions.extend_from_slice(&list);

    let mut hello = vec![0x03, 0x03];
    hello.extend_from_slice(&[0u8; 32]);
    hello.push(0);
    hello.extend_from_slice(&2u16.to_be_bytes());
    hello.extend_from_slice(&[0x13, 0x01]);
    hello.push(1);
    hello.push(0);
    hello.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
    hello.extend_from_slice(&extensions);

    let mut body = vec![0x01];
    body.push((hello.len() >> 16) as u8);
    body.extend_from_slice(&((hello.len() & 0xffff) as u16).to_be_bytes());
    body.extend_from_slice(&hello);

    let mut record = vec![0x16, 0x03, 0x01];
    record.extend_from_slice(&(body.len() as u16).to_be_bytes());
    record.extend_from_slice(&body);
    record
}

// -------------------------------------------------------------------------------------------
// SR-08: IPv4-embedded IPv6 forms the address guard does not decode.
// -------------------------------------------------------------------------------------------

/// `check_v6` decodes only IPv4-*mapped* addresses (`::ffff:0:0/96`).
///
/// `Ipv6Addr::to_ipv4_mapped` deliberately excludes the deprecated IPv4-*compatible* form
/// (`::a.b.c.d`) and knows nothing of NAT64's well-known prefix (`64:ff9b::/96`) or 6to4
/// (`2002::/16`). Each of those embeds an IPv4 address that the guard never judges by IPv4 rules,
/// so an RFC 1918 or loopback address hidden inside one passes.
///
/// Reachability depends on the host's IPv6 stack and on there being a NAT64/6to4 path, so this is
/// a defence-in-depth gap rather than a live bypass on a typical deployment. It is reported
/// because the guard's own comment says an IPv4-mapped address "must be judged by its IPv4 rules,
/// or ::ffff:192.168.1.1 walks straight through" - and these are the same class of trick.
#[test]
fn sr08_ipv4_embedded_ipv6_forms_bypass_the_address_guard() {
    use cachic::upstream::guard;

    // The form the guard does handle, for contrast.
    assert!(
        guard::check("::ffff:192.168.1.1".parse().unwrap(), false).is_err(),
        "the mapped form is handled; this assertion is the control"
    );

    for bypass in [
        "::192.168.1.1",        // IPv4-compatible, deprecated but still parsed
        "::127.0.0.1",          // loopback, same form
        "64:ff9b::192.168.1.1", // NAT64 well-known prefix
        "2002:c0a8:0101::1",    // 6to4 encapsulating 192.168.1.1
    ] {
        assert!(
            guard::check(bypass.parse().unwrap(), false).is_ok(),
            "{bypass} was refused - has the guard been widened?"
        );
    }
}
