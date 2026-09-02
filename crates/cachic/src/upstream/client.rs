//! The upstream HTTP client.
//!
//! Fetches slices from origins, resolving only through the dedicated resolver and only to
//! addresses the guard permits.
//!
//! Redirects are deliberately not followed. Following one silently would store the redirect
//! target's bytes under the original request's cache key, so a later request for that key would
//! serve content from a URL nobody asked for (FR-21).

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use hyper::{header::HeaderMap, StatusCode};

use super::resolver::UpstreamResolver;

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    /// Retries after a transient failure. One is enough to paper over a reset connection without
    /// turning an origin outage into an amplification attack against it.
    pub retries: usize,
    pub max_inflight: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(120),
            retries: 1,
            max_inflight: 256,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("building the upstream client failed: {0}")]
    Build(#[source] reqwest::Error),
    #[error(transparent)]
    Resolve(#[from] super::resolver::ResolveError),
    #[error("upstream request to {url} failed: {source}")]
    Request {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("upstream {url} answered {status}, which is not cacheable")]
    Status { url: String, status: StatusCode },
    #[error("upstream {url} redirected to {location:?}; cachic does not follow redirects")]
    Redirect { url: String, location: String },
}

/// One upstream response, with the bits the orchestrator needs.
#[derive(Debug)]
pub struct UpstreamResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl UpstreamResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// Total object length from a `Content-Range: bytes a-b/total`.
    pub fn content_range_total(&self) -> Option<u64> {
        let value = self.header("content-range")?;
        let (_, total) = value.rsplit_once('/')?;
        total.trim().parse().ok()
    }
}

/// Fetches from origins.
#[derive(Clone)]
pub struct UpstreamClient {
    http: reqwest::Client,
    resolver: Arc<UpstreamResolver>,
    inflight: Arc<tokio::sync::Semaphore>,
    config: ClientConfig,
}

impl std::fmt::Debug for UpstreamClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpstreamClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl UpstreamClient {
    pub fn new(
        resolver: Arc<UpstreamResolver>,
        config: ClientConfig,
    ) -> Result<Self, UpstreamError> {
        let http = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(config.request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(UpstreamError::Build)?;
        Ok(Self {
            http,
            resolver,
            inflight: Arc::new(tokio::sync::Semaphore::new(config.max_inflight)),
            config,
        })
    }

    /// Check that a host resolves to something we are willing to fetch from.
    ///
    /// Called before the request so a refused host fails fast and identically whether or not the
    /// origin is reachable.
    pub async fn check_host(&self, host: &str, port: u16) -> Result<(), UpstreamError> {
        self.resolver.resolve(host, port).await?;
        Ok(())
    }

    /// Check the host named by a URL.
    ///
    /// Host and port are derived from the URL rather than passed alongside it: a caller holding a
    /// `Host` header has `example.com:8080`, which is not a hostname and not an address literal,
    /// and threading that into the resolver silently turns every fetch into a failed lookup.
    pub async fn check_url(&self, url: &str) -> Result<(), UpstreamError> {
        let parsed = reqwest::Url::parse(url).map_err(|_| UpstreamError::Status {
            url: url.to_owned(),
            status: StatusCode::BAD_REQUEST,
        })?;
        let host = parsed.host_str().unwrap_or_default().to_owned();
        let port = parsed.port_or_known_default().unwrap_or(80);
        self.check_host(&host, port).await
    }

    /// Fetch a byte range from an origin.
    pub async fn fetch_range(
        &self,
        url: &str,
        headers: &HeaderMap,
        start: u64,
        end: u64,
    ) -> Result<UpstreamResponse, UpstreamError> {
        // The guard runs on every fetch, not only on the first: DNS can change under us, and a
        // long-lived object should not keep a stale permission.
        self.check_url(url).await?;

        // Backpressure rather than an unbounded queue: NFR-4 caps in-flight upstream fetches, and
        // exceeding it should slow us down, not exhaust the origin's connection limit.
        let _permit = self
            .inflight
            .acquire()
            .await
            .expect("inflight semaphore is never closed");

        let mut attempt = 0;
        loop {
            let mut request = self
                .http
                .get(url)
                .header("range", format!("bytes={start}-{end}"));
            for (name, value) in headers {
                request = request.header(name, value);
            }

            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_redirection() {
                        let location = response
                            .headers()
                            .get("location")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_owned();
                        return Err(UpstreamError::Redirect {
                            url: url.to_owned(),
                            location,
                        });
                    }
                    if !status.is_success() {
                        return Err(UpstreamError::Status {
                            url: url.to_owned(),
                            status,
                        });
                    }
                    let headers = response.headers().clone();
                    let body = response
                        .bytes()
                        .await
                        .map_err(|source| UpstreamError::Request {
                            url: url.to_owned(),
                            source,
                        })?;
                    return Ok(UpstreamResponse {
                        status,
                        headers,
                        body,
                    });
                }
                Err(source) => {
                    // Retry only transport-level failures, and only as many times as configured.
                    // A 5xx is the origin's answer, not a transport fault, and retrying it would
                    // amplify an outage.
                    if attempt < self.config.retries && (source.is_connect() || source.is_timeout())
                    {
                        attempt += 1;
                        continue;
                    }
                    return Err(UpstreamError::Request {
                        url: url.to_owned(),
                        source,
                    });
                }
            }
        }
    }

    /// Stream a whole object from an origin.
    ///
    /// Used for the `no_ranges` path (FR-13), where the origin ignores `Range` and returns the
    /// entire body. The response is streamed rather than buffered: these objects are the ones
    /// measured in tens of gigabytes, and buffering one would defeat the point of a slice store.
    ///
    /// The in-flight permit is *not* held for the life of the stream. A full-object fill can run
    /// for hours, and holding a permit that long would let a handful of range-ignoring origins
    /// starve every other fetch.
    pub async fn fetch_stream(
        &self,
        url: &str,
        headers: &HeaderMap,
    ) -> Result<
        (
            HeaderMap,
            impl futures_util::Stream<Item = reqwest::Result<Bytes>>,
        ),
        UpstreamError,
    > {
        self.check_url(url).await?;

        let mut request = self.http.get(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|source| UpstreamError::Request {
                url: url.to_owned(),
                source,
            })?;

        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_owned();
            return Err(UpstreamError::Redirect {
                url: url.to_owned(),
                location,
            });
        }
        if !status.is_success() {
            return Err(UpstreamError::Status {
                url: url.to_owned(),
                status,
            });
        }

        let headers = response.headers().clone();
        Ok((headers, response.bytes_stream()))
    }

    pub fn available_permits(&self) -> usize {
        self.inflight.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(max_inflight: usize) -> UpstreamClient {
        let resolver =
            Arc::new(UpstreamResolver::new(&["1.1.1.1".parse().unwrap()], false).unwrap());
        UpstreamClient::new(
            resolver,
            ClientConfig {
                max_inflight,
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn derives_host_and_port_from_the_url() {
        // A Host header carries "example.com:8080", which is neither a hostname nor an address
        // literal. Deriving from the URL is what stops that reaching the resolver.
        let c = client(4);
        assert!(c.check_url("http://192.168.1.1:8080/x").await.is_err());
        assert!(c.check_url("http://93.184.216.34/x").await.is_ok());
        assert!(c.check_url("not a url").await.is_err());
    }

    #[tokio::test]
    async fn refuses_a_private_host_before_making_a_request() {
        // Fails identically whether or not anything is listening, so the guard cannot be probed
        // for what exists on the LAN.
        let c = client(4);
        let err = c.check_host("192.168.1.1", 80).await.unwrap_err();
        assert!(matches!(err, UpstreamError::Resolve(_)), "{err}");
    }

    #[tokio::test]
    async fn allows_a_public_literal() {
        let c = client(4);
        c.check_host("93.184.216.34", 80).await.unwrap();
    }

    #[tokio::test]
    async fn inflight_permits_are_bounded() {
        let c = client(2);
        assert_eq!(c.available_permits(), 2);
        let _a = c.inflight.clone().acquire_owned().await.unwrap();
        let _b = c.inflight.clone().acquire_owned().await.unwrap();
        assert_eq!(c.available_permits(), 0);
    }

    #[test]
    fn parses_the_total_length_from_content_range() {
        let mut headers = HeaderMap::new();
        headers.insert("content-range", "bytes 0-1023/98765".parse().unwrap());
        let r = UpstreamResponse {
            status: StatusCode::PARTIAL_CONTENT,
            headers,
            body: Bytes::new(),
        };
        assert_eq!(r.content_range_total(), Some(98765));
    }

    #[test]
    fn a_malformed_content_range_yields_no_length_rather_than_a_wrong_one() {
        for value in ["bytes 0-1023", "nonsense", "bytes */", "bytes 0-1/abc"] {
            let mut headers = HeaderMap::new();
            headers.insert("content-range", value.parse().unwrap());
            let r = UpstreamResponse {
                status: StatusCode::PARTIAL_CONTENT,
                headers,
                body: Bytes::new(),
            };
            assert_eq!(r.content_range_total(), None, "value {value:?}");
        }
    }
}
