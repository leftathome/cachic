//! The client-facing HTTP server.
//!
//! Accepts requests for any `Host`, matches them to a service, and streams the answer out of the
//! orchestrator.
//!
//! The body pipeline is where FR-31 is honoured. Each slice fetch is spawned as a detached task
//! and the stream awaits its `JoinHandle`; dropping the stream when a client disconnects drops
//! the handle, not the task, so the fill completes and the slice is stored. Awaiting the futures
//! inline - which is what the M0 spike did - cancels them on drop and throws away work that a
//! later client would have benefited from.

use std::{convert::Infallible, net::SocketAddr, sync::Arc};

use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::{combinators::UnsyncBoxBody, BodyExt, Full, StreamBody};
use hyper::{
    body::Frame,
    header::{
        HeaderValue, ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG,
        LAST_MODIFIED, RANGE,
    },
    server::conn::http1,
    service::service_fn,
    Method, Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::{
    config::rules::Rules,
    orchestrator::{payload_window, Orchestrator, OrchestratorError},
    proxy::{headers, heartbeat},
    services::key::{self, CompiledRule},
};

/// Response bodies are unsync-boxed: foyer's `get_or_fetch` future is `!Sync`, so a body that
/// awaits it cannot satisfy `BodyExt::boxed`'s `Sync` bound. hyper does not require `Sync` bodies.
type BoxedBody = UnsyncBoxBody<Bytes, std::io::Error>;

pub struct ServerConfig {
    pub orchestrator: Arc<Orchestrator>,
    /// Access-log format. `Json` is the supported output; `Lancache` emits monolithic's
    /// `cachelog` so existing dashboards keep working (FR-52).
    pub log_format: crate::config::LogFormat,
    /// Metrics, if telemetry is wired. Absent in tests that do not care.
    pub metrics: Option<Arc<crate::telemetry::metrics::Metrics>>,
    /// The live domain list. Read per request through an `ArcSwap`, so a refresh takes effect
    /// without restarting and without a lock on the hot path.
    pub services: Arc<crate::services::refresh::LiveServices>,
    pub rules: Arc<Rules>,
    pub compiled: Arc<std::collections::HashMap<String, CompiledRule>>,
    pub hostname: String,
    pub passthrough_unknown_hosts: bool,
    /// Bounds simultaneously open client connections (NFR-4).
    pub connections: Arc<crate::proxy::limits::ConnectionLimit>,
    /// Tracks in-flight requests so shutdown can wait for them (FR-62).
    pub drain: Arc<crate::proxy::shutdown::Drain>,
}

impl ServerConfig {
    /// Defaults for tests and callers that do not care about limits.
    /// Defaults for tests and callers that do not refresh their service list.
    pub fn with_defaults(
        orchestrator: Arc<Orchestrator>,
        list: crate::services::domains::DomainList,
        hostname: impl Into<String>,
    ) -> Self {
        Self::with_services(
            orchestrator,
            crate::services::refresh::LiveServices::new(list),
            hostname,
        )
    }

    /// Build with a live, refreshable service list.
    pub fn with_services(
        orchestrator: Arc<Orchestrator>,
        services: Arc<crate::services::refresh::LiveServices>,
        hostname: impl Into<String>,
    ) -> Self {
        Self {
            orchestrator,
            services,
            rules: Arc::new(Rules::default()),
            compiled: Arc::new(std::collections::HashMap::new()),
            hostname: hostname.into(),
            passthrough_unknown_hosts: false,
            connections: crate::proxy::limits::ConnectionLimit::new(10_000),
            drain: crate::proxy::shutdown::Drain::new(),
            log_format: crate::config::LogFormat::Json,
            metrics: None,
        }
    }
}

pub struct Server {
    addr: SocketAddr,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}

impl Server {
    pub async fn bind(listen: SocketAddr, config: Arc<ServerConfig>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(listen).await?;
        let addr = listener.local_addr()?;
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let task_shutdown = shutdown.clone();

        tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    r = listener.accept() => r,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                        if task_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        continue;
                    }
                };
                let Ok((stream, _)) = accepted else { continue };

                // Refuse rather than queue at the connection limit. A queued connection looks
                // alive to the client while making no progress, and a game client that times out
                // silently is worse than one that fails fast and retries.
                let Some(permit) = config.connections.try_acquire() else {
                    tracing::warn!(
                        open = config.connections.open(),
                        max = config.connections.max(),
                        "refusing connection: at the connection limit"
                    );
                    drop(stream);
                    continue;
                };

                let peer = stream
                    .peer_addr()
                    .map(|a| a.ip().to_string())
                    .unwrap_or_else(|_| "-".into());
                let io = TokioIo::new(stream);
                let config = config.clone();
                tokio::spawn(async move {
                    // The permit is released when this task ends, however it ends.
                    let _permit = permit;
                    if let Some(metrics) = &config.metrics {
                        metrics.connections.inc();
                    }
                    let connection_metrics = config.metrics.clone();
                    let service = service_fn(move |req| handle(req, config.clone(), peer.clone()));
                    // Errors here are client disconnects; the proxy does not care.
                    let _ = http1::Builder::new().serve_connection(io, service).await;
                    if let Some(metrics) = &connection_metrics {
                        metrics.connections.dec();
                    }
                });
            }
        });

        Ok(Self { addr, shutdown })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

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
    config: Arc<ServerConfig>,
    client_ip: String,
) -> Result<Response<BoxedBody>, Infallible> {
    // Refuse new requests once draining, so a keep-alive connection cannot extend the drain
    // indefinitely by issuing request after request.
    let Some(_guard) = config.drain.enter() else {
        return Ok(text(
            StatusCode::SERVICE_UNAVAILABLE,
            "cachic is shutting down\n",
        ));
    };

    let started = std::time::Instant::now();
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let range = req
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let user_agent = req
        .headers()
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let response = match serve(req, config.clone()).await {
        Ok(response) => response,
        Err(e) => {
            tracing::warn!(error = %e, "request failed");
            text(StatusCode::BAD_GATEWAY, &format!("upstream error: {e}\n"))
        }
    };

    // One access event per served request (FR-51, FR-52). Emitted here rather than in `serve` so
    // that error responses are logged too - a cache whose failures are invisible is worse than
    // one with no log at all.
    let status = response.status().as_u16();
    let service = response
        .headers()
        .get("x-cachic-service")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_string();
    let cache_status = response
        .headers()
        .get("x-cache")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_string();
    let bytes = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    if let Some(metrics) = &config.metrics {
        metrics
            .requests
            .with_label_values(&[&service, &cache_status])
            .inc();
        metrics
            .bytes_served
            .with_label_values(&[&service, &cache_status])
            .inc_by(bytes);
    }

    crate::telemetry::logs::AccessEvent {
        client_ip,
        service,
        host,
        method,
        path,
        range,
        status,
        bytes,
        cache_status,
        upstream_seconds: started.elapsed().as_secs_f64(),
        user_agent,
        timestamp: crate::telemetry::logs::clf_timestamp(crate::store::index::now_secs()),
    }
    .emit(config.log_format);

    let mut response = response;
    strip_internal_headers(&mut response);
    Ok(response)
}

/// Remove the internal header the access log uses to attribute a request.
///
/// It exists only to carry the service name from `serve` to `handle`; a client has no business
/// seeing our internal routing decisions.
fn strip_internal_headers(response: &mut Response<BoxedBody>) {
    response.headers_mut().remove("x-cachic-service");
}

async fn serve(
    req: Request<hyper::body::Incoming>,
    config: Arc<ServerConfig>,
) -> Result<Response<BoxedBody>, OrchestratorError> {
    let path = req.uri().path().to_owned();

    if heartbeat::is_heartbeat(&path) {
        return Ok(heartbeat::respond(Response::builder(), &config.hostname)
            .body(empty())
            .unwrap());
    }

    // Everything but GET and HEAD is proxied uncached; the cached path is GET and HEAD only
    // (FR-05). Pass-through for other methods is TASK-18's concern.
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return Ok(text(
            StatusCode::METHOD_NOT_ALLOWED,
            "cachic caches GET and HEAD\n",
        ));
    }
    let is_head = req.method() == Method::HEAD;

    let host = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();

    // One read of the live matcher per request. A refresh swaps the pointer; a request that
    // started before the swap finishes against the list it started with.
    let matcher = config.services.matcher();
    let Some(service) = matcher.service_for(&host) else {
        // Unmatched hosts are 404 by default. Proxying them uncached would make this an open
        // proxy on the LAN (FR-02, FR-64).
        if config.passthrough_unknown_hosts {
            return Ok(text(
                StatusCode::NOT_IMPLEMENTED,
                "passthrough is not implemented until TASK-18\n",
            ));
        }
        return Ok(text(
            StatusCode::NOT_FOUND,
            "no cached service matches this host\n",
        ));
    };
    let service = service.to_owned();

    let default_rule = CompiledRule::default();
    let rule = config.compiled.get(&service).unwrap_or(&default_rule);

    let target = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");

    if !rule.is_cacheable(&path) {
        return Ok(text(
            StatusCode::NOT_IMPLEMENTED,
            "uncached pass-through is not implemented until TASK-18\n",
        ));
    }

    let cache_key = key::normalise(&service, &host, target, rule);
    let scheme = if rule.upstream_https { "https" } else { "http" };
    let url = format!("{scheme}://{host}{target}");
    let forwarded = headers::forwarded_request_headers(req.headers());
    let raw_range = req
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let if_range = req
        .headers()
        .get("if-range")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let orchestrator = config.orchestrator.clone();

    // If-Range means "send the range if the entity is unchanged, otherwise send the whole
    // object" (FR-17). Resolving it needs the object's current validators, so it is applied
    // after planning, by re-planning without the range on a mismatch.
    let mut effective_range = raw_range.clone();
    let plan = match orchestrator
        .plan(&cache_key, &url, &forwarded, effective_range.as_deref())
        .await
    {
        Ok(plan) => plan,
        Err(OrchestratorError::Unsatisfiable { total_len }) => {
            return Ok(Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, format!("bytes */{total_len}"))
                .header("x-lancache-processed-by", config.hostname.clone())
                .body(empty())
                .unwrap());
        }
        Err(e) => return Err(e),
    };

    let plan = match &if_range {
        Some(header)
            if !crate::orchestrator::validators::if_range_matches(
                header,
                &crate::orchestrator::validators::Validators::new(
                    plan.meta.etag.clone(),
                    plan.meta.last_modified.clone(),
                ),
            ) =>
        {
            // The entity changed, so the client's range refers to bytes that may no longer be
            // there. Answer with the whole object, which is what If-Range asks for.
            effective_range = None;
            orchestrator
                .plan(&cache_key, &url, &forwarded, None)
                .await?
        }
        _ => plan,
    };
    let _ = &effective_range;

    let body_len = if plan.total_len == 0 {
        0
    } else {
        plan.wanted.len()
    };

    let mut builder = Response::builder()
        .status(plan.status)
        .header(CONTENT_LENGTH, body_len.to_string())
        .header(ACCEPT_RANGES, "bytes")
        .header("x-cache", plan.cache_status.as_str())
        .header("x-lancache-processed-by", config.hostname.clone())
        // Internal: read back by the access log so it can attribute the request to a service
        // without re-running the matcher. Stripped before the response leaves the process.
        .header("x-cachic-service", service.clone());
    if plan.partial && plan.total_len > 0 {
        builder = builder.header(
            CONTENT_RANGE,
            format!(
                "bytes {}-{}/{}",
                plan.wanted.start, plan.wanted.end, plan.total_len
            ),
        );
    }
    for (name, value) in [
        (CONTENT_TYPE, plan.meta.content_type.clone()),
        (ETAG, plan.meta.etag.clone()),
        (LAST_MODIFIED, plan.meta.last_modified.clone()),
    ] {
        if let Some(v) = value.and_then(|v| HeaderValue::from_str(&v).ok()) {
            builder = builder.header(name, v);
        }
    }

    if is_head || body_len == 0 {
        return Ok(builder.body(empty()).unwrap());
    }

    // Speculative prefetch, if this client is streaming. Fired before the body so the prefetch
    // and the response race rather than queue.
    orchestrator.maybe_prefetch(&plan, &url, &forwarded);

    let indices = orchestrator.indices(&plan);
    let readahead = orchestrator.readahead();

    // Each slice is fetched by a detached task, so a client disconnect drops the JoinHandle
    // rather than the fetch (FR-31). `buffered` preserves order and caps concurrency at the
    // read-ahead window, so per-connection memory is bounded by construction.
    let stream = futures_util::stream::iter(indices)
        .map(move |index| {
            let orchestrator = orchestrator.clone();
            let plan = plan.clone();
            let url = url.clone();
            let forwarded = forwarded.clone();
            let handle = tokio::spawn(async move {
                let window = orchestrator.window(&plan, index);
                let slice_url = url.clone();
                let value = orchestrator
                    .clone()
                    .slice(plan, url, forwarded, index)
                    .await?;
                payload_window(&value, window).map_err(|_| OrchestratorError::ShortSlice {
                    url: slice_url,
                    index,
                    expected: window.1.saturating_sub(window.0) as u64,
                    actual: value.payload.len(),
                })
            });
            async move {
                match handle.await {
                    Ok(Ok(bytes)) => Ok(bytes),
                    Ok(Err(e)) => Err(std::io::Error::other(e.to_string())),
                    // A panicked slice task must fail the response rather than truncate it: a
                    // short body that looks successful is worse than an error.
                    Err(join) => Err(std::io::Error::other(format!("slice task failed: {join}"))),
                }
            }
        })
        .buffered(readahead)
        .map(|r| r.map(Frame::data));

    Ok(builder
        .body(BodyExt::boxed_unsync(StreamBody::new(stream)))
        .unwrap())
}
