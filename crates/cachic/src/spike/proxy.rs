//! The spike proxy: probe, plan, pipeline, serve.
//!
//! This is the shape the M1 orchestrator will take, built quickly to answer the M0 questions.
//! Known omissions, all deliberate and all owned by later tasks:
//!
//! - Object metadata lives in a `DashMap`, not redb (TASK-11).
//! - A `no_ranges` origin is filled to completion before the client is served; the streaming
//!   object-level filler is TASK-16.
//! - A validator mismatch fails the request instead of bumping the generation (TASK-17).
//! - Client disconnect cancels the in-flight pipeline, which FR-31 forbids (TASK-18).
//! - There is no service matching or key normalisation; the request path is the key (TASK-08).

use std::{
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use futures_util::{StreamExt, TryStreamExt};
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, StreamBody};
use hyper::{
    body::Frame,
    header::{
        ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, LAST_MODIFIED, RANGE,
    },
    server::conn::http1,
    service::service_fn,
    Method, Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use super::{
    range::{self, RangeError},
    slice::{object_id, ObjectId, SliceHeader, SliceKey, SliceValue},
    store::{SpikeStore, StoreConfig},
};

/// Header naming the cache that served a response, as the lancache ecosystem expects.
const HDR_PROCESSED_BY: &str = "X-LanCache-Processed-By";
const HDR_CACHE: &str = "X-Cache";

#[derive(Debug, Clone)]
pub struct SpikeConfig {
    pub listen: SocketAddr,
    /// Base URL of the origin, e.g. `http://127.0.0.1:8080`.
    pub origin: String,
    pub slice_size: u32,
    /// Bounded read-ahead window. Per-connection memory is `readahead * slice_size`.
    pub readahead: usize,
    pub data_dir: PathBuf,
    pub store: StoreConfig,
    pub upstream_timeout: Duration,
}

impl SpikeConfig {
    pub fn new(origin: impl Into<String>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            origin: origin.into(),
            slice_size: 1024 * 1024,
            readahead: 4,
            data_dir: data_dir.into(),
            store: StoreConfig::default(),
            upstream_timeout: Duration::from_secs(30),
        }
    }
}

/// What the proxy knows about an object, learned by probing.
#[derive(Debug, Clone)]
struct ObjectMeta {
    total_len: u64,
    generation: u32,
    etag: Option<String>,
    last_modified: Option<String>,
    content_type: Option<String>,
    /// The origin ignored `Range` and sent the whole object (FR-13).
    no_ranges: bool,
}

#[derive(Debug, Default)]
pub struct ProxyStats {
    pub requests: AtomicU64,
    pub probes: AtomicU64,
    pub upstream_slice_fetches: AtomicU64,
    pub full_object_fills: AtomicU64,
}

struct Inner {
    store: SpikeStore,
    client: reqwest::Client,
    origin: String,
    slice_size: u32,
    readahead: usize,
    /// Object metadata, single-flighted per object: a cold object hit by N clients
    /// must probe once, not N times.
    objects: DashMap<ObjectId, Arc<tokio::sync::OnceCell<ObjectMeta>>>,
    stats: Arc<ProxyStats>,
    hostname: String,
}

/// A running spike proxy. Dropping the handle stops it.
pub struct SpikeProxy {
    inner: Arc<Inner>,
    addr: SocketAddr,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

impl SpikeProxy {
    pub async fn start(config: SpikeConfig) -> anyhow::Result<Self> {
        let store = SpikeStore::open(&config.data_dir, &config.store).await?;
        let client = reqwest::Client::builder()
            .timeout(config.upstream_timeout)
            // Redirects are not followed: doing so silently would cache content under a key that
            // does not describe it (FR-21). The spike simply refuses them.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        let inner = Arc::new(Inner {
            store,
            client,
            origin: config.origin.trim_end_matches('/').to_string(),
            slice_size: config.slice_size,
            readahead: config.readahead.max(1),
            objects: DashMap::new(),
            stats: Arc::new(ProxyStats::default()),
            hostname: hostname(),
        });

        let listener = TcpListener::bind(config.listen).await?;
        let addr = listener.local_addr()?;
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let task_inner = inner.clone();
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
                let inner = task_inner.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req| handle(req, inner.clone()));
                    let _ = http1::Builder::new().serve_connection(io, service).await;
                });
            }
        });

        Ok(Self {
            inner,
            addr,
            shutdown,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn stats(&self) -> &ProxyStats {
        &self.inner.stats
    }

    pub async fn close(&self) -> anyhow::Result<()> {
        self.inner.store.close().await
    }
}

impl Drop for SpikeProxy {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "cachic".to_string())
}

/// Response bodies are unsync-boxed: foyer's `get_or_fetch` future is `!Sync`, so a body that
/// awaits it cannot satisfy `BodyExt::boxed`'s `Sync` bound. hyper does not require `Sync` bodies.
type BoxedBody = UnsyncBoxBody<Bytes, std::io::Error>;

fn empty() -> BoxedBody {
    Full::new(Bytes::new())
        .map_err(|e: Infallible| match e {})
        .boxed_unsync()
}

fn text(status: StatusCode, message: &str) -> Response<BoxedBody> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(
            Full::new(Bytes::from(message.to_owned()))
                .map_err(|e: Infallible| match e {})
                .boxed_unsync(),
        )
        .unwrap()
}

async fn handle(
    req: Request<hyper::body::Incoming>,
    inner: Arc<Inner>,
) -> Result<Response<BoxedBody>, Infallible> {
    inner.stats.requests.fetch_add(1, Ordering::Relaxed);
    match serve(req, inner).await {
        Ok(response) => Ok(response),
        Err(e) => {
            tracing::warn!(error = %e, "request failed");
            Ok(text(
                StatusCode::BAD_GATEWAY,
                &format!("upstream error: {e}\n"),
            ))
        }
    }
}

async fn serve(
    req: Request<hyper::body::Incoming>,
    inner: Arc<Inner>,
) -> anyhow::Result<Response<BoxedBody>> {
    // The heartbeat is an ecosystem contract: prefill tools probe it to decide whether a cache
    // is present (FR-07).
    if req.uri().path() == "/lancache-heartbeat" {
        return Ok(Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(HDR_PROCESSED_BY, inner.hostname.clone())
            .header("Access-Control-Allow-Origin", "*")
            .body(empty())
            .unwrap());
    }

    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        // Pass-through for other methods is TASK-09; the spike is only about the cached path.
        return Ok(text(
            StatusCode::METHOD_NOT_ALLOWED,
            "spike proxy serves GET and HEAD only\n",
        ));
    }
    let is_head = req.method() == Method::HEAD;

    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "/".into());
    let object = object_id(&path);

    let raw_range = req
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    // Parse before probing: a multi-range or malformed header means "serve the whole object",
    // which changes which slice we probe with.
    let spec = match raw_range.as_deref().map(range::parse_range) {
        None => None,
        Some(Ok(spec)) => Some(spec),
        // RFC 9110: a Range that cannot be parsed is ignored, and multi-range may be answered in full.
        Some(Err(RangeError::Malformed | RangeError::Multiple)) => None,
        Some(Err(RangeError::Unsatisfiable)) => unreachable!("parse never yields Unsatisfiable"),
    };

    let probe_index = match spec {
        // A suffix range cannot be resolved without the length, so probe slice 0.
        Some(range::RangeSpec::Suffix(_)) | None => 0,
        Some(range::RangeSpec::FromTo(start, _)) | Some(range::RangeSpec::From(start)) => {
            (start / inner.slice_size as u64) as u32
        }
    };

    // Clone the cell out before awaiting: holding a DashMap guard across an await deadlocks.
    let cell = inner
        .objects
        .entry(object)
        .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new()))
        .clone();
    // Whether this request is the one paying for the probe decides how it reports X-Cache: the
    // slice the probe pulls in is part of serving *this* request, not evidence of a prior one.
    let probed_here = !cell.initialized();
    let meta = cell
        .get_or_try_init(|| probe(&inner, &path, object, probe_index))
        .await?
        .clone();

    let wanted = match spec {
        None => match range::whole(meta.total_len) {
            Some(r) => r,
            None => {
                // Zero-length object (FR-15).
                return Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header(CONTENT_LENGTH, "0")
                    .header(HDR_CACHE, "HIT")
                    .header(HDR_PROCESSED_BY, inner.hostname.clone())
                    .body(empty())
                    .unwrap());
            }
        },
        Some(spec) => match range::resolve(spec, meta.total_len) {
            Ok(r) => r,
            Err(_) => {
                return Ok(Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(CONTENT_RANGE, format!("bytes */{}", meta.total_len))
                    .header(HDR_PROCESSED_BY, inner.hostname.clone())
                    .body(empty())
                    .unwrap());
            }
        },
    };
    let partial = spec.is_some();

    // A range-ignoring origin has to be filled whole before it can be sliced. The streaming
    // filler that serves while filling is TASK-16.
    if meta.no_ranges {
        fill_whole_object(&inner, &path, object, &meta).await?;
    }

    let plan = range::plan(wanted, inner.slice_size);

    // Classification only; racy by construction and never used to decide what to fetch.
    let mut resident = plan
        .indices()
        .filter(|i| {
            inner
                .store
                .contains(&SliceKey::new(object, meta.generation, *i))
        })
        .count() as u32;
    if probed_here && plan.first <= probe_index && probe_index <= plan.last {
        // The probe fetched this slice moments ago on this request's behalf.
        resident = resident.saturating_sub(1);
    }
    let cache_status = if resident == plan.count() {
        "HIT"
    } else if resident == 0 {
        "MISS"
    } else {
        "PARTIAL"
    };

    let mut builder = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(CONTENT_LENGTH, wanted.len().to_string())
        .header(ACCEPT_RANGES, "bytes")
        .header(HDR_CACHE, cache_status)
        .header(HDR_PROCESSED_BY, inner.hostname.clone());
    if partial {
        builder = builder.header(
            CONTENT_RANGE,
            format!("bytes {}-{}/{}", wanted.start, wanted.end, meta.total_len),
        );
    }
    if let Some(v) = &meta.content_type {
        builder = builder.header(CONTENT_TYPE, v.clone());
    }
    if let Some(v) = &meta.etag {
        builder = builder.header(ETAG, v.clone());
    }
    if let Some(v) = &meta.last_modified {
        builder = builder.header(LAST_MODIFIED, v.clone());
    }

    if is_head {
        return Ok(builder.body(empty()).unwrap());
    }

    let readahead = inner.readahead;
    let slice_size = inner.slice_size;

    // The ordered, bounded pipeline. `buffered` preserves order and caps concurrency at the
    // read-ahead window, so per-connection memory is `readahead * slice_size` by construction
    // rather than by a separate limiter.
    let stream = futures_util::stream::iter(plan.indices().collect::<Vec<_>>())
        .map(move |index| {
            let inner = inner.clone();
            let path = path.clone();
            let meta = meta.clone();
            async move {
                let value = fetch_or_load_slice(&inner, &path, object, &meta, index).await?;
                let (from, to) = range::payload_window(index, slice_size, wanted);
                let payload = &value.payload;
                if to > payload.len() || from > to {
                    anyhow::bail!(
                        "slice {index} is {} bytes, wanted window {from}..{to}",
                        payload.len()
                    );
                }
                Ok::<Bytes, anyhow::Error>(payload.slice(from..to))
            }
        })
        .buffered(readahead)
        .map_ok(Frame::data)
        .map_err(|e| std::io::Error::other(e.to_string()));

    Ok(builder
        .body(BodyExt::boxed_unsync(StreamBody::new(stream)))
        .unwrap())
}

/// Learn an object's length and validators by fetching one slice.
async fn probe(
    inner: &Arc<Inner>,
    path: &str,
    object: ObjectId,
    index: u32,
) -> anyhow::Result<ObjectMeta> {
    inner.stats.probes.fetch_add(1, Ordering::Relaxed);

    let start = index as u64 * inner.slice_size as u64;
    let end = start + inner.slice_size as u64 - 1;
    let url = format!("{}{}", inner.origin, path);
    let response = inner
        .client
        .get(&url)
        .header(RANGE, format!("bytes={start}-{end}"))
        .send()
        .await?;

    let status = response.status();
    let headers = response.headers().clone();
    let etag = header_string(&headers, ETAG.as_str());
    let last_modified = header_string(&headers, LAST_MODIFIED.as_str());
    let content_type = header_string(&headers, CONTENT_TYPE.as_str());

    if status == StatusCode::PARTIAL_CONTENT {
        let total = header_string(&headers, CONTENT_RANGE.as_str())
            .and_then(|v| parse_content_range_total(&v))
            .ok_or_else(|| anyhow::anyhow!("206 without a usable Content-Range"))?;
        let meta = ObjectMeta {
            total_len: total,
            generation: 0,
            etag,
            last_modified,
            content_type,
            no_ranges: false,
        };
        // The probe already paid for these bytes; keep them.
        let body = response.bytes().await?;
        let value = SliceValue::new(header_for(&meta, inner.slice_size), body);
        let key = SliceKey::new(object, meta.generation, index);
        inner
            .store
            .get_or_fetch(key, move || async move { Ok(value) })
            .await?;
        inner
            .stats
            .upstream_slice_fetches
            .fetch_add(1, Ordering::Relaxed);
        return Ok(meta);
    }

    if status == StatusCode::RANGE_NOT_SATISFIABLE {
        // The probe index is past the end of the object. The length still comes back in
        // `Content-Range: bytes */total`, which is all we need to answer the client with a 416.
        let total = header_string(&headers, CONTENT_RANGE.as_str())
            .and_then(|v| parse_content_range_total(&v))
            .ok_or_else(|| anyhow::anyhow!("416 without a usable Content-Range"))?;
        return Ok(ObjectMeta {
            total_len: total,
            generation: 0,
            etag,
            last_modified,
            content_type,
            no_ranges: false,
        });
    }

    if status.is_success() {
        // The origin ignored the range (FR-13). Length comes from Content-Length; the body is
        // discarded here and refetched by the filler, which is wasteful and is exactly why
        // TASK-16 exists.
        let total = response
            .content_length()
            .ok_or_else(|| anyhow::anyhow!("200 without Content-Length; cannot slice"))?;
        return Ok(ObjectMeta {
            total_len: total,
            generation: 0,
            etag,
            last_modified,
            content_type,
            no_ranges: true,
        });
    }

    anyhow::bail!("probe failed: upstream returned {status}")
}

fn header_for(meta: &ObjectMeta, slice_size: u32) -> SliceHeader {
    SliceHeader {
        slice_size,
        total_len: meta.total_len,
        generation: meta.generation,
        etag: meta.etag.clone(),
        last_modified: meta.last_modified.clone(),
        content_type: meta.content_type.clone(),
    }
}

fn header_string(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Extract the total length from `bytes a-b/total`.
fn parse_content_range_total(value: &str) -> Option<u64> {
    let (_, total) = value.rsplit_once('/')?;
    total.trim().parse().ok()
}

/// Fetch one slice, coalescing with any concurrent request for the same slice.
async fn fetch_or_load_slice(
    inner: &Arc<Inner>,
    path: &str,
    object: ObjectId,
    meta: &ObjectMeta,
    index: u32,
) -> anyhow::Result<SliceValue> {
    let key = SliceKey::new(object, meta.generation, index);
    let extent = range::slice_extent(index, inner.slice_size, meta.total_len);
    let url = format!("{}{}", inner.origin, path);
    let client = inner.client.clone();
    let header = header_for(meta, inner.slice_size);
    let expected_etag = meta.etag.clone();
    let stats = inner.stats.clone();

    inner
        .store
        .get_or_fetch(key, move || async move {
            stats.upstream_slice_fetches.fetch_add(1, Ordering::Relaxed);
            let response = client
                .get(&url)
                .header(RANGE, format!("bytes={}-{}", extent.start, extent.end))
                .send()
                .await?;
            if response.status() != StatusCode::PARTIAL_CONTENT {
                anyhow::bail!(
                    "slice {index}: expected 206, upstream returned {}",
                    response.status()
                );
            }
            // A validator change mid-object means the bytes we already hold describe a version
            // that no longer exists. The spike fails the request; TASK-17 bumps the generation
            // and aborts the client stream so the client retries against the new version.
            let etag = header_string(response.headers(), ETAG.as_str());
            if etag != expected_etag {
                anyhow::bail!(
                    "slice {index}: validator changed mid-object ({expected_etag:?} -> {etag:?})"
                );
            }
            let body = response.bytes().await?;
            if body.len() as u64 != extent.len() {
                anyhow::bail!(
                    "slice {index}: expected {} bytes, upstream sent {}",
                    extent.len(),
                    body.len()
                );
            }
            Ok(SliceValue::new(header, body))
        })
        .await
}

/// Stream a whole object from a range-ignoring origin, cutting it into slices as it arrives.
///
/// Memory is bounded to one slice; latency is not bounded at all, because the client waits for
/// the entire fill. TASK-16 replaces this with a filler that publishes per-slice readiness.
async fn fill_whole_object(
    inner: &Arc<Inner>,
    path: &str,
    object: ObjectId,
    meta: &ObjectMeta,
) -> anyhow::Result<()> {
    let slice_size = inner.slice_size as usize;
    let last_index = if meta.total_len == 0 {
        0
    } else {
        ((meta.total_len - 1) / inner.slice_size as u64) as u32
    };
    // Already filled?
    if (0..=last_index).all(|i| {
        inner
            .store
            .contains(&SliceKey::new(object, meta.generation, i))
    }) {
        return Ok(());
    }

    inner
        .stats
        .full_object_fills
        .fetch_add(1, Ordering::Relaxed);
    let url = format!("{}{}", inner.origin, path);
    let response = inner.client.get(&url).send().await?;
    if !response.status().is_success() {
        anyhow::bail!("full fetch failed: upstream returned {}", response.status());
    }

    let mut stream = response.bytes_stream();
    let mut buf = BytesMut::with_capacity(slice_size);
    let mut index = 0u32;
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk?);
        while buf.len() >= slice_size {
            let payload = buf.split_to(slice_size).freeze();
            store_slice(inner, object, meta, index, payload).await?;
            index += 1;
        }
    }
    if !buf.is_empty() {
        store_slice(inner, object, meta, index, buf.freeze()).await?;
    }
    Ok(())
}

async fn store_slice(
    inner: &Arc<Inner>,
    object: ObjectId,
    meta: &ObjectMeta,
    index: u32,
    payload: Bytes,
) -> anyhow::Result<()> {
    let key = SliceKey::new(object, meta.generation, index);
    let value = SliceValue::new(header_for(meta, inner.slice_size), payload);
    inner
        .store
        .get_or_fetch(key, move || async move { Ok(value) })
        .await?;
    Ok(())
}
