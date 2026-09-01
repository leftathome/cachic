//! Verifies the mock origin itself. Everything downstream trusts these behaviours, so they are
//! asserted directly rather than assumed.

use std::time::Duration;

use cachic_testkit::{
    content,
    mockcdn::{Config, MockCdn, RangeBehaviour},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Minimal HTTP/1.1 client: enough to issue one request and read the whole response.
/// Deliberately not reqwest, so the testkit has no opinion about the proxy's client stack.
async fn request(addr: std::net::SocketAddr, req: &str) -> (String, Vec<u8>) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("no header terminator");
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let body = decode_body(&head, &buf[split + 4..]);
    (head, body)
}

/// The origin streams, so responses come back chunked unless the body is empty.
fn decode_body(head: &str, raw: &[u8]) -> Vec<u8> {
    if !head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return raw.to_vec();
    }
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < raw.len() {
        let line_end = match raw[pos..].windows(2).position(|w| w == b"\r\n") {
            Some(p) => pos + p,
            None => break,
        };
        let size_str = String::from_utf8_lossy(&raw[pos..line_end]);
        let size = usize::from_str_radix(size_str.trim(), 16).unwrap_or(0);
        pos = line_end + 2;
        if size == 0 {
            break;
        }
        out.extend_from_slice(&raw[pos..pos + size]);
        pos += size + 2;
    }
    out
}

#[tokio::test]
async fn serves_full_object_matching_the_generator() {
    let cdn = MockCdn::start(Config::default()).await.unwrap();
    let size = 10_000u64;
    let path = MockCdn::object_path("alpha", size);

    let (head, body) = request(
        cdn.addr(),
        &format!("GET {path} HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"),
    )
    .await;

    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(body.len() as u64, size);
    assert_eq!(
        body,
        content::range(content::seed_for("alpha"), 0, size as usize)
    );
}

#[tokio::test]
async fn honours_single_ranges() {
    let cdn = MockCdn::start(Config::default()).await.unwrap();
    let size = 10_000u64;
    let path = MockCdn::object_path("beta", size);
    let seed = content::seed_for("beta");

    for (start, end) in [(0u64, 0u64), (0, 1023), (999, 2048), (9_000, 9_999)] {
        let (head, body) = request(
            cdn.addr(),
            &format!(
                "GET {path} HTTP/1.1\r\nHost: origin\r\nRange: bytes={start}-{end}\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert!(
            head.contains(&format!("content-range: bytes {start}-{end}/{size}")),
            "{head}"
        );
        let expected = content::range(seed, start, (end - start + 1) as usize);
        assert_eq!(body, expected, "range {start}-{end}");
    }
}

#[tokio::test]
async fn rejects_unsatisfiable_ranges_with_416() {
    let cdn = MockCdn::start(Config::default()).await.unwrap();
    let path = MockCdn::object_path("gamma", 1_000);
    let (head, _) = request(
        cdn.addr(),
        &format!("GET {path} HTTP/1.1\r\nHost: origin\r\nRange: bytes=5000-6000\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 416"), "{head}");
    assert!(head.contains("content-range: bytes */1000"), "{head}");
}

#[tokio::test]
async fn range_ignoring_mode_returns_the_whole_object() {
    // This is the origin behaviour that forces the no_ranges path (FR-13).
    let cdn = MockCdn::start(Config {
        range_behaviour: RangeBehaviour::Ignore,
        ..Config::default()
    })
    .await
    .unwrap();
    let size = 5_000u64;
    let path = MockCdn::object_path("delta", size);

    let (head, body) = request(
        cdn.addr(),
        &format!(
            "GET {path} HTTP/1.1\r\nHost: origin\r\nRange: bytes=0-99\r\nConnection: close\r\n\r\n"
        ),
    )
    .await;

    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(body.len() as u64, size);
    assert!(head.contains("accept-ranges: none"), "{head}");
}

#[tokio::test]
async fn head_returns_metadata_without_a_body() {
    let cdn = MockCdn::start(Config::default()).await.unwrap();
    let path = MockCdn::object_path("epsilon", 4_242);
    let (head, body) = request(
        cdn.addr(),
        &format!("HEAD {path} HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert!(head.contains("content-length: 4242"), "{head}");
    assert!(body.is_empty());
}

#[tokio::test]
async fn counts_requests_for_coalescing_assertions() {
    let cdn = MockCdn::start(Config::default()).await.unwrap();
    let path = MockCdn::object_path("zeta", 1_000);
    assert_eq!(cdn.stats().requests(), 0);
    for _ in 0..3 {
        request(
            cdn.addr(),
            &format!("GET {path} HTTP/1.1\r\nHost: origin\r\nRange: bytes=0-99\r\nConnection: close\r\n\r\n"),
        )
        .await;
    }
    assert_eq!(cdn.stats().requests(), 3);
    assert_eq!(cdn.stats().range_requests(), 3);
    assert_eq!(cdn.stats().bytes_served(), 300);
}

#[tokio::test]
async fn first_byte_delay_is_observable() {
    let cdn = MockCdn::start(Config {
        first_byte_delay: Some(Duration::from_millis(120)),
        ..Config::default()
    })
    .await
    .unwrap();
    let path = MockCdn::object_path("eta", 100);
    let start = std::time::Instant::now();
    request(
        cdn.addr(),
        &format!("GET {path} HTTP/1.1\r\nHost: origin\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(start.elapsed() >= Duration::from_millis(100));
}
