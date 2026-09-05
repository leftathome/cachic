//! The admin API: stats, services, purge, reload, drain (FR-54).
//!
//! nginx gave operators no way to ask the cache anything. This is where "what is in there, and
//! can you drop it" gets an answer.
//!
//! Two security properties are structural rather than advisory. The API is on a port separate
//! from the data plane, and configuration refuses to start if they collide. And when a token is
//! configured, authentication fails closed: a malformed header, a missing header and a wrong
//! token are all rejected identically.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    proxy::shutdown::Drain,
    store::{hybrid::SliceStore, index::ObjectIndex, slice::SliceKey, space},
};

/// Optional bearer token.
///
/// Absent means the API is unauthenticated, which is only safe because it is bound to loopback or
/// a cluster network by default. Present means every request must carry it.
#[derive(Debug, Clone, Default)]
pub struct AuthToken(Option<Arc<str>>);

impl AuthToken {
    pub fn none() -> Self {
        Self(None)
    }

    pub fn new(token: impl AsRef<str>) -> Self {
        let token = token.as_ref();
        if token.is_empty() {
            // An empty token is a configuration mistake that would otherwise mean "no auth"
            // while looking like "auth". Treat it as absent and let the caller notice.
            return Self(None);
        }
        Self(Some(Arc::from(token)))
    }

    pub fn is_set(&self) -> bool {
        self.0.is_some()
    }

    /// Whether a request is authorised.
    ///
    /// Comparison is constant-time in the length of the configured token, so a timing signal
    /// cannot be used to recover it byte by byte.
    /// Whether a token was configured at all.
    pub fn is_configured(&self) -> bool {
        self.0.is_some()
    }

    pub fn permits(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = &self.0 else {
            return true;
        };
        let Some(value) = headers.get("authorization").and_then(|v| v.to_str().ok()) else {
            return false;
        };
        let Some(presented) = value.strip_prefix("Bearer ") else {
            return false;
        };
        constant_time_eq(presented.as_bytes(), expected.as_bytes())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The API's state, populated once the store is open.
///
/// The admin port binds before the store, so `/healthz` answers immediately rather than refusing
/// connections while a large cache directory opens - a liveness probe that gets connection-refused
/// will restart the pod, and restarting is exactly wrong when the process is mid-recovery. Until
/// the store exists, the operator endpoints report 503 rather than pretending.
#[derive(Clone, Default)]
pub struct LateApiState(Arc<std::sync::OnceLock<ApiState>>);

impl LateApiState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish the state. Returns false if it was already set.
    pub fn set(&self, state: ApiState) -> bool {
        self.0.set(state).is_ok()
    }

    pub fn get(&self) -> Option<&ApiState> {
        self.0.get()
    }
}

#[derive(Clone)]
pub struct ApiState {
    pub store: SliceStore,
    pub index: Arc<ObjectIndex>,
    pub drain: Arc<Drain>,
    pub readiness: Arc<super::Readiness>,
    pub token: AuthToken,
    pub services: Arc<Vec<ServiceInfo>>,
    pub data_dir: std::path::PathBuf,
    pub configured_disk_bytes: u64,
    pub min_free_bytes: u64,
    pub slice_size: u32,
    /// The admin API is bound somewhere a client could reach, so the destructive endpoints need a
    /// token rather than relying on the address to confine them.
    pub mutations_need_token: bool,
}

/// Refuse a destructive request that nothing is authenticating.
///
/// `/purge` empties the cache and `/drain` takes the instance out of service, both in one request.
/// While the admin API bound every interface with no token by default, either was available to any
/// client that could reach the port (SR-01). Binding to loopback is the primary fix; this is the
/// backstop for an operator who binds it wider - as the Helm chart must, so Prometheus can scrape
/// - without setting a token.
///
/// Health and metrics are deliberately not covered. They are why the port is reachable at all, and
/// neither changes anything.
fn refuse_unauthenticated_mutation(state: &ApiState) -> Option<axum::response::Response> {
    if state.mutations_need_token && !state.token.is_configured() {
        return Some(
            (
                StatusCode::FORBIDDEN,
                "refusing: this endpoint changes state, the admin API is not bound to loopback, \
                 and ADMIN_TOKEN is not set. Set ADMIN_TOKEN, or bind the admin API to \
                 loopback with ADMIN_BIND.\n",
            )
                .into_response(),
        );
    }
    None
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceInfo {
    pub name: String,
    pub patterns: usize,
}

#[derive(Debug, Serialize)]
pub struct Stats {
    pub objects: usize,
    pub indexed_bytes: u64,
    pub configured_disk_bytes: u64,
    pub effective_disk_bytes: u64,
    /// True when the free-space guard is holding the cap below what was configured (FR-46).
    pub disk_guard_engaged: bool,
    pub filesystem_total_bytes: u64,
    pub filesystem_available_bytes: u64,
    pub min_free_bytes: u64,
    pub slice_size: u32,
    pub draining: bool,
    pub inflight: u64,
}

#[derive(Debug, Deserialize)]
pub struct PurgeParams {
    /// Purge every object whose normalised key starts with this.
    pub prefix: Option<String>,
    /// Purge everything. Required explicitly, so a missing prefix cannot silently empty the cache.
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Serialize)]
pub struct PurgeResult {
    pub objects_removed: usize,
    pub slices_removed: u64,
}

fn unauthorised() -> axum::response::Response {
    (StatusCode::UNAUTHORIZED, "unauthorised\n").into_response()
}

fn starting() -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "the store is still opening\n",
    )
        .into_response()
}

pub async fn stats(
    State(late): State<LateApiState>,
    headers: HeaderMap,
) -> axum::response::Response {
    let Some(state) = late.get() else {
        return starting();
    };
    if !state.token.permits(&headers) {
        return unauthorised();
    }
    let objects = state.index.len().unwrap_or(0);
    let indexed_bytes = state.index.total_bytes().unwrap_or(0);
    let disk = space::read(&state.data_dir).unwrap_or(space::DiskSpace {
        total: 0,
        available: 0,
    });
    let effective = space::effective_cap(
        state.configured_disk_bytes,
        state.min_free_bytes,
        indexed_bytes,
        disk,
    );
    Json(Stats {
        objects,
        indexed_bytes,
        configured_disk_bytes: state.configured_disk_bytes,
        effective_disk_bytes: effective,
        disk_guard_engaged: space::is_engaged(state.configured_disk_bytes, effective),
        filesystem_total_bytes: disk.total,
        filesystem_available_bytes: disk.available,
        min_free_bytes: state.min_free_bytes,
        slice_size: state.slice_size,
        draining: state.drain.is_draining(),
        inflight: state.drain.inflight(),
    })
    .into_response()
}

pub async fn services(
    State(late): State<LateApiState>,
    headers: HeaderMap,
) -> axum::response::Response {
    let Some(state) = late.get() else {
        return starting();
    };
    if !state.token.permits(&headers) {
        return unauthorised();
    }
    Json(state.services.as_ref().clone()).into_response()
}

pub async fn purge(
    State(late): State<LateApiState>,
    headers: HeaderMap,
    Query(params): Query<PurgeParams>,
) -> axum::response::Response {
    let Some(state) = late.get() else {
        return starting();
    };
    if !state.token.permits(&headers) {
        return unauthorised();
    }
    if let Some(refusal) = refuse_unauthenticated_mutation(state) {
        return refusal;
    }

    let ids = match (&params.prefix, params.all) {
        (Some(prefix), _) => state.index.ids_with_prefix(prefix),
        // `all` must be explicit. Without it, a request that meant to carry a prefix and lost it
        // would empty the entire cache.
        (None, true) => state.index.all_ids(),
        (None, false) => {
            return (
                StatusCode::BAD_REQUEST,
                "specify ?prefix=... or ?all=true\n",
            )
                .into_response()
        }
    };

    let ids = match ids {
        Ok(ids) => ids,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot read the index: {e}\n"),
            )
                .into_response()
        }
    };

    let mut slices_removed = 0u64;
    for id in &ids {
        // Slices and the index entry are removed together. Leaving slices behind would waste the
        // space the operator was trying to reclaim; leaving the index entry behind would let a
        // later request believe the object is known.
        if let Ok(Some(meta)) = state.index.get(id) {
            let count = if meta.total_len == 0 {
                0
            } else {
                meta.total_len.div_ceil(state.slice_size as u64)
            };
            for index in 0..count {
                state
                    .store
                    .remove(&SliceKey::new(*id, meta.generation, index as u32));
                slices_removed += 1;
            }
        }
        let _ = state.index.remove(id);
    }

    Json(PurgeResult {
        objects_removed: ids.len(),
        slices_removed,
    })
    .into_response()
}

pub async fn drain(
    State(late): State<LateApiState>,
    headers: HeaderMap,
) -> axum::response::Response {
    let Some(state) = late.get() else {
        return starting();
    };
    if !state.token.permits(&headers) {
        return unauthorised();
    }
    if let Some(refusal) = refuse_unauthenticated_mutation(state) {
        return refusal;
    }
    // Readiness fails immediately so traffic moves away; the process keeps serving what is
    // already in flight. Actual exit is the operator's or the orchestrator's decision.
    state.readiness.begin_drain();
    (StatusCode::ACCEPTED, "draining\n").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bearer(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("authorization", format!("Bearer {token}").parse().unwrap());
        h
    }

    #[test]
    fn no_token_permits_everything() {
        assert!(AuthToken::none().permits(&HeaderMap::new()));
    }

    #[test]
    fn a_configured_token_is_required() {
        let token = AuthToken::new("s3cret");
        assert!(token.permits(&bearer("s3cret")));
        assert!(
            !token.permits(&HeaderMap::new()),
            "missing header was accepted"
        );
        assert!(!token.permits(&bearer("wrong")));
    }

    #[test]
    fn authentication_fails_closed_on_malformed_headers() {
        let token = AuthToken::new("s3cret");
        let mut h = HeaderMap::new();
        h.insert("authorization", "s3cret".parse().unwrap());
        assert!(
            !token.permits(&h),
            "a bare token without the Bearer scheme was accepted"
        );

        let mut h = HeaderMap::new();
        h.insert("authorization", "Basic s3cret".parse().unwrap());
        assert!(!token.permits(&h));

        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer ".parse().unwrap());
        assert!(!token.permits(&h));
    }

    #[test]
    fn an_empty_configured_token_is_treated_as_absent() {
        // Otherwise it looks like authentication is on while permitting everything.
        let token = AuthToken::new("");
        assert!(!token.is_set());
        assert!(token.permits(&HeaderMap::new()));
    }

    #[test]
    fn token_comparison_is_length_safe() {
        let token = AuthToken::new("s3cret");
        assert!(!token.permits(&bearer("s3cre")));
        assert!(!token.permits(&bearer("s3crets")));
        assert!(!token.permits(&bearer("")));
    }

    #[test]
    fn constant_time_eq_matches_ordinary_equality() {
        // Byte literals rather than words, since this compares opaque secrets, not prose.
        assert!(constant_time_eq(&[1, 2, 3], &[1, 2, 3]));
        assert!(!constant_time_eq(&[1, 2, 3], &[1, 2, 4]));
        assert!(!constant_time_eq(&[1, 2, 3], &[1, 2]));
        assert!(constant_time_eq(&[], &[]));
    }
}
