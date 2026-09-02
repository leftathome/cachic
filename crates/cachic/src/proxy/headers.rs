//! Request and response header handling.
//!
//! Two responsibilities that are easy to get subtly wrong:
//!
//! - **Hop-by-hop headers must not be forwarded** (RFC 9110 section 7.6.1). Forwarding
//!   `Connection` or `Transfer-Encoding` upstream produces protocol errors that look like
//!   upstream flakiness.
//! - **Upstream entity headers must be preserved** on cached objects (FR-06). `ETag` and
//!   `Last-Modified` in particular are load-bearing: they are how a validator change is detected
//!   and a generation bumped.

use hyper::header::{HeaderMap, HeaderName, HeaderValue};

/// Headers that apply to a single transport connection and must never be forwarded.
///
/// `Connection` itself may also name further headers to drop, which is handled separately.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Response headers describing the entity, which must survive caching (FR-06).
const PRESERVED_ENTITY: &[&str] = &[
    "content-type",
    "etag",
    "last-modified",
    "cache-control",
    "content-encoding",
    "content-language",
    "expires",
];

/// `X-Cache` values (FR-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheStatus {
    /// Every slice came from the store.
    Hit,
    /// No slice came from the store.
    Miss,
    /// Some did, some did not.
    Partial,
    /// Not cacheable; proxied through.
    Bypass,
}

impl CacheStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CacheStatus::Hit => "HIT",
            CacheStatus::Miss => "MISS",
            CacheStatus::Partial => "PARTIAL",
            CacheStatus::Bypass => "BYPASS",
        }
    }

    /// Classify from how many of the planned slices were already resident.
    pub fn classify(resident: u32, total: u32) -> Self {
        if total == 0 || resident == total {
            CacheStatus::Hit
        } else if resident == 0 {
            CacheStatus::Miss
        } else {
            CacheStatus::Partial
        }
    }
}

/// Headers named by a `Connection` header, which are hop-by-hop for this message only.
fn connection_named(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all("connection")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn is_hop_by_hop(name: &HeaderName, connection_named: &[String]) -> bool {
    let lower = name.as_str().to_ascii_lowercase();
    HOP_BY_HOP.contains(&lower.as_str()) || connection_named.contains(&lower)
}

/// Headers to send upstream: everything the client sent except hop-by-hop ones.
///
/// `User-Agent` is deliberately forwarded - several services vary their response on it, and
/// stripping it makes them behave differently through the cache than without it (FR-06).
pub fn forwarded_request_headers(client: &HeaderMap) -> HeaderMap {
    let named = connection_named(client);
    let mut out = HeaderMap::with_capacity(client.len());
    for (name, value) in client {
        if is_hop_by_hop(name, &named) {
            continue;
        }
        // Host is set by the upstream client from the URL; carrying the client's would send the
        // cache's own hostname to the origin.
        if name.as_str().eq_ignore_ascii_case("host") {
            continue;
        }
        // Range is decided by the orchestrator per slice, not copied from the client.
        if name.as_str().eq_ignore_ascii_case("range") {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

/// Entity headers worth storing with an object and replaying on a hit.
pub fn preserved_response_headers(upstream: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for name in PRESERVED_ENTITY {
        for value in upstream.get_all(*name) {
            if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
                out.append(name, value.clone());
            }
        }
    }
    out
}

/// Add the headers the lancache ecosystem expects on every response (FR-07).
pub fn add_cache_headers(headers: &mut HeaderMap, status: CacheStatus, hostname: &str) {
    headers.insert("x-cache", HeaderValue::from_static("")); // replaced below
    headers.insert(
        "x-cache",
        HeaderValue::from_str(status.as_str()).unwrap_or(HeaderValue::from_static("MISS")),
    );
    if let Ok(value) = HeaderValue::from_str(hostname) {
        headers.insert("x-lancache-processed-by", value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn strips_hop_by_hop_headers() {
        let client = headers(&[
            ("user-agent", "Valve/Steam HTTP Client 1.0"),
            ("connection", "keep-alive"),
            ("transfer-encoding", "chunked"),
            ("accept", "*/*"),
        ]);
        let out = forwarded_request_headers(&client);
        assert!(out.get("connection").is_none());
        assert!(out.get("transfer-encoding").is_none());
        assert_eq!(out.get("accept").unwrap(), "*/*");
    }

    #[test]
    fn forwards_the_user_agent() {
        // Several services vary on it; stripping it makes them behave differently through the
        // cache than without it.
        let client = headers(&[("user-agent", "Valve/Steam HTTP Client 1.0")]);
        let out = forwarded_request_headers(&client);
        assert_eq!(
            out.get("user-agent").unwrap(),
            "Valve/Steam HTTP Client 1.0"
        );
    }

    #[test]
    fn strips_headers_named_by_connection() {
        // RFC 9110: Connection can nominate further headers as hop-by-hop for this message.
        let client = headers(&[
            ("connection", "x-custom, keep-alive"),
            ("x-custom", "secret"),
            ("accept", "*/*"),
        ]);
        let out = forwarded_request_headers(&client);
        assert!(
            out.get("x-custom").is_none(),
            "connection-named header leaked"
        );
        assert!(out.get("accept").is_some());
    }

    #[test]
    fn drops_host_and_range() {
        // Host would send the cache's own name upstream; Range is decided per slice.
        let client = headers(&[("host", "cache.lan"), ("range", "bytes=0-99")]);
        let out = forwarded_request_headers(&client);
        assert!(out.get("host").is_none());
        assert!(out.get("range").is_none());
    }

    #[test]
    fn preserves_entity_headers() {
        let upstream = headers(&[
            ("etag", "\"abc\""),
            ("last-modified", "Wed, 21 Oct 2015 07:28:00 GMT"),
            ("content-type", "application/octet-stream"),
            ("server", "nginx"),
            ("date", "now"),
        ]);
        let out = preserved_response_headers(&upstream);
        assert_eq!(out.get("etag").unwrap(), "\"abc\"");
        assert!(out.get("last-modified").is_some());
        assert!(out.get("content-type").is_some());
        // Not entity headers; not our business to replay.
        assert!(out.get("server").is_none());
        assert!(out.get("date").is_none());
    }

    #[test]
    fn classifies_cache_status() {
        assert_eq!(CacheStatus::classify(4, 4), CacheStatus::Hit);
        assert_eq!(CacheStatus::classify(0, 4), CacheStatus::Miss);
        assert_eq!(CacheStatus::classify(2, 4), CacheStatus::Partial);
        // A zero-length object is a hit, not a miss: there is nothing to fetch.
        assert_eq!(CacheStatus::classify(0, 0), CacheStatus::Hit);
    }

    #[test]
    fn adds_the_ecosystem_headers() {
        let mut h = HeaderMap::new();
        add_cache_headers(&mut h, CacheStatus::Hit, "cache01");
        assert_eq!(h.get("x-cache").unwrap(), "HIT");
        assert_eq!(h.get("x-lancache-processed-by").unwrap(), "cache01");
    }
}
