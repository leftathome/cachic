//! Upstream fetches connect to the address `UPSTREAM_DNS` returned, not the system resolver's
//! (FR-03, FR-64).
//!
//! This is the test the RC deployment found missing. Every earlier test resolved a *literal*
//! address, where the resolver has nothing to do, so the defect was invisible: the client resolved
//! through `UpstreamResolver` to satisfy the guard, threw the answer away, and let reqwest resolve
//! the hostname a second time through the system resolver and connect *there*. The guard was
//! inspecting one address while the socket went to another.
//!
//! Proving that requires the two resolvers to *disagree*, which is why there is a DNS server in
//! here. `UpstreamResolver` speaks DNS to its configured servers and nothing else injects an
//! answer into it, so a mock server is the only way to hand it one.
//!
//! The names are under `.test`, reserved by RFC 6761 and never resolvable by the system. That
//! makes the disagreement total - one resolver has an answer and the other does not - which is
//! the sharpest form available here. `localhost` looks like the obvious choice and is not usable:
//! hickory short-circuits the `localhost.` zone internally, so both resolvers would agree on
//! 127.0.0.1 and the test would assert nothing. The first version of this file made exactly that
//! mistake and passed against the unfixed code.
//!
//! A decoy origin listens on 127.0.0.1 at the *same port* as the real one, so a fetch that
//! resolves through the wrong path and still reaches something is caught rather than mistaken for
//! success.

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use cachic::upstream::{
    client::{ClientConfig, UpstreamClient},
    resolver::{GuardedResolver, UpstreamResolver},
};
use cachic_testkit::mockdns::MockDns;
use hyper::header::HeaderMap;

/// The address the mock DNS server hands out.
const GUARDED: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);
/// Where anything resolving through the system would land.
const SYSTEM: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);
/// A name no system resolver can answer (RFC 6761).
const NAME: &str = "origin.cachic.test";

struct Origin {
    hits: Arc<AtomicU64>,
}

/// Serve `body` on `addr` until dropped.
async fn origin(addr: SocketAddr, body: &'static str) -> std::io::Result<Origin> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let hits = Arc::new(AtomicU64::new(0));
    let task_hits = hits.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            task_hits.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\
                     Content-Range: bytes 0-{}/{}\r\n\r\n{body}",
                    body.len(),
                    body.len().saturating_sub(1),
                    body.len(),
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    Ok(Origin { hits })
}

/// Two origins on the same port, one at each address.
///
/// Same port on purpose: if they differed, the URL alone would decide which was reached and the
/// test would prove nothing about the resolver.
async fn origin_pair() -> (u16, Origin, Origin) {
    for _ in 0..32 {
        // Take an ephemeral port on the guarded address, then try to claim the same port on the
        // system address. A port free on one loopback address is nearly always free on the other;
        // if something holds it, take another.
        let probe = tokio::net::TcpListener::bind((GUARDED, 0)).await.unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let Ok(guarded) = origin(SocketAddr::from((GUARDED, port)), "guarded-resolver").await
        else {
            continue;
        };
        let Ok(system) = origin(SocketAddr::from((SYSTEM, port)), "system-resolver").await else {
            continue;
        };
        return (port, guarded, system);
    }
    panic!("could not claim the same port on both loopback addresses");
}

fn client(resolver: Arc<UpstreamResolver>) -> UpstreamClient {
    UpstreamClient::new(
        resolver,
        ClientConfig {
            request_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(2),
            ..ClientConfig::default()
        },
    )
    .unwrap()
}

/// A resolver that answers every name with [`GUARDED`], and the server backing it.
///
/// The `MockDns` is returned rather than dropped: dropping it stops the server.
async fn guarded_resolver(allow_private: bool) -> (Arc<UpstreamResolver>, MockDns) {
    let dns = MockDns::start(GUARDED).await.unwrap();
    let resolver = UpstreamResolver::with_servers(&[dns.addr()], allow_private).unwrap();
    (Arc::new(resolver), dns)
}

#[tokio::test]
async fn a_fetch_connects_to_the_address_upstream_dns_returned() {
    let (port, guarded, system) = origin_pair().await;
    let (resolver, dns) = guarded_resolver(true).await;

    // Guard against the tautology that made the first version of this test worthless. If the
    // resolver were answering from anywhere but the mock server, the two paths would not be
    // distinguishable and everything below would pass regardless of the wiring.
    let resolved = resolver.resolve(NAME, port).await.unwrap();
    assert_eq!(
        resolved,
        vec![SocketAddr::from((GUARDED, port))],
        "the mock DNS server is not the authority for this lookup, so this test proves nothing"
    );
    assert!(dns.queries() > 0, "the mock DNS server was never queried");

    let c = client(resolver);
    let url = format!("http://{NAME}:{port}/object");
    let response = c
        .fetch_range("test", &url, &HeaderMap::new(), 0, 15)
        .await
        .expect("a name served only by UPSTREAM_DNS was not fetchable");

    assert_eq!(
        std::str::from_utf8(response.body.as_ref()).unwrap(),
        "guarded-resolver"
    );
    assert_eq!(guarded.hits.load(Ordering::Relaxed), 1);
    assert_eq!(
        system.hits.load(Ordering::Relaxed),
        0,
        "the fetch dialled 127.0.0.1, which UPSTREAM_DNS never named"
    );
}

#[tokio::test]
async fn the_guard_applies_to_the_address_that_is_dialled() {
    // The security half. While reqwest resolved independently, the guard was advisory: a DNS
    // server answering public to UPSTREAM_DNS and private to the system resolver defeated FR-64
    // completely. With allow_private off, a loopback answer must refuse the fetch outright.
    let (port, guarded, system) = origin_pair().await;
    let (resolver, _dns) = guarded_resolver(false).await;
    let c = client(resolver);

    let url = format!("http://{NAME}:{port}/object");
    let result = c.fetch_range("test", &url, &HeaderMap::new(), 0, 15).await;

    assert!(result.is_err(), "a loopback upstream was fetched");
    assert_eq!(
        guarded.hits.load(Ordering::Relaxed) + system.hits.load(Ordering::Relaxed),
        0,
        "the guard refused the address but a connection was made anyway"
    );
}

#[tokio::test]
async fn the_resolver_adapter_refuses_what_the_guard_refuses() {
    use reqwest::dns::{Name, Resolve};

    let (resolver, _dns) = guarded_resolver(false).await;
    let adapter = GuardedResolver::new(resolver);
    let name: Name = NAME.parse().unwrap();
    assert!(
        adapter.resolve(name).await.is_err(),
        "the adapter returned an address the guard refuses"
    );
}
