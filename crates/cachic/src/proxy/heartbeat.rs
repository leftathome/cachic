//! The lancache heartbeat endpoint.
//!
//! `GET /lancache-heartbeat` is an ecosystem contract, not a health check of ours: SteamPrefill,
//! the Epic and Battle.net prefill tools and LANCache Manager probe it to decide whether a cache
//! is present on the network. Its shape is not ours to redesign, and getting it wrong means those
//! tools silently bypass the cache (FR-07, G4).
//!
//! Our own liveness and readiness live on the admin port (FR-53); this is separate and public.

use hyper::{header::HeaderValue, http::response::Builder, StatusCode};

pub const PATH: &str = "/lancache-heartbeat";

/// Whether a request target is the heartbeat.
pub fn is_heartbeat(path: &str) -> bool {
    path == PATH
}

/// Apply the heartbeat response: 204 with the identifying header and permissive CORS.
///
/// The CORS headers are required because LANCache Manager probes this from a browser context.
pub fn respond(builder: Builder, hostname: &str) -> Builder {
    let mut builder = builder
        .status(StatusCode::NO_CONTENT)
        .header("access-control-allow-origin", "*")
        .header("access-control-allow-headers", "*")
        .header("access-control-expose-headers", "*");
    if let Ok(value) = HeaderValue::from_str(hostname) {
        builder = builder.header("x-lancache-processed-by", value);
    }
    builder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_heartbeat_path_exactly() {
        assert!(is_heartbeat("/lancache-heartbeat"));
        // Prefill tools request this exact path; being loose here would shadow a real object.
        assert!(!is_heartbeat("/lancache-heartbeat/"));
        assert!(!is_heartbeat("/lancache-heartbeat?x=1"));
        assert!(!is_heartbeat("/other"));
    }

    #[test]
    fn responds_204_with_the_expected_headers() {
        let response = respond(hyper::Response::builder(), "cache01")
            .body(())
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let h = response.headers();
        assert_eq!(h.get("x-lancache-processed-by").unwrap(), "cache01");
        // LANCache Manager probes from a browser, so CORS must be permissive.
        assert_eq!(h.get("access-control-allow-origin").unwrap(), "*");
    }
}
