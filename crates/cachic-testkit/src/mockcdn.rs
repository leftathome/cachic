//! Mock CDN origin.
//!
//! Serves deterministic content (see [`crate::content`]) over HTTP with configurable behaviour,
//! so the proxy can be tested against the awkward origins it will meet in the field: ones that
//! ignore `Range`, ones that are slow, ones that fail.
//!
//! Request counters are the mechanism by which request coalescing (FR-30) is verified: N
//! concurrent clients against a cold object must not produce N upstream fetches per slice.

use std::{
    convert::Infallible,
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
use hyper::{
    body::Frame,
    header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, RANGE},
    server::conn::http1,
    service::service_fn,
    Method, Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::content;

/// Chunk size the origin streams bodies in.
const STREAM_CHUNK: usize = 256 * 1024;

/// How the origin responds to `Range` requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeBehaviour {
    /// Honour single byte ranges with `206` and a `Content-Range`.
    Honour,
    /// Ignore `Range` entirely and always return `200` with the full body. Real CDNs do this, and
    /// it is the case that forces the `no_ranges` path (FR-13).
    Ignore,
}

/// Observable counters. The coalescing assertions read these.
#[derive(Debug, Default)]
pub struct Stats {
    pub requests: AtomicU64,
    pub range_requests: AtomicU64,
    pub bytes_served: AtomicU64,
}

impl Stats {
    pub fn requests(&self) -> u64 {
        self.requests.load(Ordering::Relaxed)
    }
    pub fn range_requests(&self) -> u64 {
        self.range_requests.load(Ordering::Relaxed)
    }
    pub fn bytes_served(&self) -> u64 {
        self.bytes_served.load(Ordering::Relaxed)
    }
    pub fn reset(&self) {
        self.requests.store(0, Ordering::Relaxed);
        self.range_requests.store(0, Ordering::Relaxed);
        self.bytes_served.store(0, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub range_behaviour: RangeBehaviour,
    /// Appended to every ETag. Changing it at runtime simulates the object being replaced
    /// upstream, which is what forces a generation bump (FR-14).
    pub etag_suffix: Arc<std::sync::atomic::AtomicU64>,
    /// Artificial delay before the first byte, to emulate a WAN origin.
    pub first_byte_delay: Option<Duration>,
    /// Delay inserted between streamed chunks, to emulate a bandwidth-limited origin.
    pub chunk_delay: Option<Duration>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            range_behaviour: RangeBehaviour::Honour,
            etag_suffix: Arc::new(AtomicU64::new(0)),
            first_byte_delay: None,
            chunk_delay: None,
        }
    }
}

/// A running mock origin. Dropping the handle stops the server.
pub struct MockCdn {
    addr: SocketAddr,
    stats: Arc<Stats>,
    shutdown: Arc<AtomicBool>,
    config_etag_suffix: Arc<AtomicU64>,
}

impl MockCdn {
    /// Bind to an ephemeral port on loopback and start serving.
    pub async fn start(config: Config) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let stats = Arc::new(Stats::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let etag_suffix = config.etag_suffix.clone();

        let task_stats = stats.clone();
        let task_shutdown = shutdown.clone();
        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    r = listener.accept() => r,
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {
                        if task_shutdown.load(Ordering::Relaxed) {
                            return;
                        }
                        continue;
                    }
                };
                let (stream, _) = match accepted {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let io = TokioIo::new(stream);
                let stats = task_stats.clone();
                let config = config.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(req, stats.clone(), config.clone()));
                    // Errors here are client disconnects; the origin does not care.
                    let _ = http1::Builder::new().serve_connection(io, service).await;
                });
            }
        });

        Ok(Self {
            addr,
            stats,
            shutdown,
            config_etag_suffix: etag_suffix,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Replace every object's ETag, simulating the content being changed upstream.
    pub fn change_validators(&self) {
        self.config_etag_suffix.fetch_add(1, Ordering::Relaxed);
    }

    /// URL for an object of `size` bytes whose content is derived from `name`.
    pub fn object_url(&self, name: &str, size: u64) -> String {
        format!("{}/o/{}/{}", self.base_url(), name, size)
    }

    /// Path component of [`Self::object_url`], for proxies that take the path only.
    pub fn object_path(name: &str, size: u64) -> String {
        format!("/o/{name}/{size}")
    }

    pub fn stats(&self) -> &Arc<Stats> {
        &self.stats
    }
}

impl Drop for MockCdn {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

/// Parse `/o/<name>/<size>` into the object's seed and length.
fn parse_path(path: &str) -> Option<(u64, u64)> {
    let rest = path.strip_prefix("/o/")?;
    let (name, size) = rest.rsplit_once('/')?;
    let size: u64 = size.parse().ok()?;
    Some((content::seed_for(name), size))
}

/// Parse a single `bytes=a-b` range against a known object length.
/// Returns `None` for syntactically invalid or unsatisfiable ranges.
fn parse_single_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let spec = value.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    match (start.trim(), end.trim()) {
        ("", suffix) => {
            let n: u64 = suffix.parse().ok()?;
            if n == 0 || total == 0 {
                return None;
            }
            let n = n.min(total);
            Some((total - n, total - 1))
        }
        (s, "") => {
            let start: u64 = s.parse().ok()?;
            if start >= total {
                return None;
            }
            Some((start, total - 1))
        }
        (s, e) => {
            let start: u64 = s.parse().ok()?;
            let end: u64 = e.parse().ok()?;
            if start > end || start >= total {
                return None;
            }
            Some((start, end.min(total - 1)))
        }
    }
}

fn etag_for(seed: u64, size: u64, suffix: u64) -> String {
    format!("\"{seed:016x}-{size:x}-{suffix:x}\"")
}

type BoxedBody = BoxBody<Bytes, Infallible>;

fn empty() -> BoxedBody {
    Full::new(Bytes::new()).boxed()
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    stats: Arc<Stats>,
    config: Config,
) -> Result<Response<BoxedBody>, Infallible> {
    stats.requests.fetch_add(1, Ordering::Relaxed);

    let (seed, size) = match parse_path(req.uri().path()) {
        Some(v) => v,
        None => {
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(empty())
                .unwrap())
        }
    };

    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return Ok(Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(empty())
            .unwrap());
    }
    let is_head = req.method() == Method::HEAD;

    let raw_range = req
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if raw_range.is_some() {
        stats.range_requests.fetch_add(1, Ordering::Relaxed);
    }

    let honour_ranges = config.range_behaviour == RangeBehaviour::Honour;
    let requested = raw_range
        .as_deref()
        .filter(|_| honour_ranges)
        .map(|r| (r, parse_single_range(r, size)));

    // A syntactically valid but unsatisfiable range must be a 416, not a silent full body.
    if let Some((raw, None)) = requested {
        if raw.starts_with("bytes=") && !raw.contains(',') {
            return Ok(Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, format!("bytes */{size}"))
                .body(empty())
                .unwrap());
        }
    }

    let (start, end, partial) = match requested {
        Some((_, Some((s, e)))) => (s, e, true),
        _ => (0, size.saturating_sub(1), false),
    };
    let len = if size == 0 { 0 } else { end - start + 1 };

    if let Some(d) = config.first_byte_delay {
        tokio::time::sleep(d).await;
    }

    let mut builder = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(CONTENT_TYPE, "application/octet-stream")
        .header(CONTENT_LENGTH, len.to_string())
        .header(
            ETAG,
            etag_for(seed, size, config.etag_suffix.load(Ordering::Relaxed)),
        )
        .header(ACCEPT_RANGES, if honour_ranges { "bytes" } else { "none" });
    if partial {
        builder = builder.header(CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }

    if is_head || len == 0 {
        return Ok(builder.body(empty()).unwrap());
    }

    stats.bytes_served.fetch_add(len, Ordering::Relaxed);

    let chunk_delay = config.chunk_delay;
    let stream = async_stream::stream! {
        let mut sent = 0u64;
        while sent < len {
            if let Some(d) = chunk_delay {
                tokio::time::sleep(d).await;
            }
            let n = std::cmp::min(STREAM_CHUNK as u64, len - sent) as usize;
            let chunk = content::range(seed, start + sent, n);
            sent += n as u64;
            yield Ok::<_, Infallible>(Frame::data(Bytes::from(chunk)));
        }
    };

    Ok(builder.body(StreamBody::new(stream).boxed()).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_object_paths() {
        assert_eq!(
            parse_path("/o/game/1000"),
            Some((content::seed_for("game"), 1000))
        );
        assert_eq!(parse_path("/nope"), None);
        assert_eq!(parse_path("/o/game/notanumber"), None);
    }

    #[test]
    fn parses_ranges() {
        assert_eq!(parse_single_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_single_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_single_range("bytes=-100", 1000), Some((900, 999)));
        // Clamped to the object, not rejected: RFC 9110 allows an over-long end.
        assert_eq!(parse_single_range("bytes=900-5000", 1000), Some((900, 999)));
        // Unsatisfiable and malformed cases.
        assert_eq!(parse_single_range("bytes=1000-1001", 1000), None);
        assert_eq!(parse_single_range("bytes=50-10", 1000), None);
        assert_eq!(parse_single_range("bytes=0-10,20-30", 1000), None);
        assert_eq!(parse_single_range("items=0-10", 1000), None);
    }
}
