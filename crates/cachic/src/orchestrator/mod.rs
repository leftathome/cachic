//! Request orchestration: probe, slice plan, ordered pipeline.
//!
//! Where a client request becomes a set of slice fetches. Everything cachic claims over nginx
//! lives here: coalescing that streams the fill in progress rather than blocking on a lock,
//! bounded read-ahead, and fills that outlive the connection that started them.
//!
//! Three invariants, each learned the hard way:
//!
//! 1. **Object metadata is single-flighted.** The M0 spike probed once per concurrent client
//!    until a test counted upstream requests and found N probes for N clients. Metadata now goes
//!    through a per-object `OnceCell`.
//! 2. **Fills outlive their connection** (FR-31). Each slice fetch is a detached task, so a
//!    client hanging up mid-download does not abandon a partially-filled slice. Attaching the
//!    fetch to the response stream would cancel it on drop.
//! 3. **Back-pressure is the read-ahead window.** Slice futures are awaited in order within a
//!    bounded window, so per-connection memory is `readahead * slice_size` by construction rather
//!    than by a limiter bolted on afterwards.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use bytes::Bytes;
use hyper::{header::HeaderMap, StatusCode};

use crate::{
    proxy::{
        headers::CacheStatus,
        range::{self, ByteRange},
    },
    services::key::CacheKey,
    store::{
        hybrid::SliceStore,
        index::{now_secs, ObjectIndex, ObjectMeta},
        slice::{ObjectId, SliceHeader, SliceKey, SliceValue},
    },
    upstream::client::UpstreamClient,
};

/// What the orchestrator decided about a request, before any body is produced.
#[derive(Debug, Clone)]
pub struct Plan {
    pub status: StatusCode,
    pub wanted: ByteRange,
    pub total_len: u64,
    pub cache_status: CacheStatus,
    pub meta: ObjectMeta,
    pub object: ObjectId,
    pub partial: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum OrchestratorError {
    #[error("range is not satisfiable for a {total_len}-byte object")]
    Unsatisfiable { total_len: u64 },
    #[error(transparent)]
    Upstream(#[from] crate::upstream::client::UpstreamError),
    #[error(transparent)]
    Store(#[from] crate::store::hybrid::StoreError),
    #[error("upstream did not report a length for {url}, so the object cannot be sliced")]
    NoLength { url: String },
    #[error("slice {index} of {url}: expected {expected} bytes, upstream sent {actual}")]
    ShortSlice {
        url: String,
        index: u32,
        expected: u64,
        actual: usize,
    },
    #[error("validators for {url} changed mid-object ({had:?} -> {now:?}); aborting so the client retries")]
    ValidatorChanged {
        url: String,
        had: Option<String>,
        now: Option<String>,
    },
    #[error("the object-level fill path is not implemented until TASK-16 (origin ignores Range)")]
    NoRangesUnsupported,
}

/// Shared orchestration state.
pub struct Orchestrator {
    store: SliceStore,
    index: Arc<ObjectIndex>,
    upstream: UpstreamClient,
    slice_size: u32,
    readahead: usize,
    /// Per-object metadata single-flight. Without this, N concurrent clients probe N times.
    metadata: Mutex<HashMap<ObjectId, Arc<tokio::sync::OnceCell<ObjectMeta>>>>,
}

impl Orchestrator {
    pub fn new(
        store: SliceStore,
        index: Arc<ObjectIndex>,
        upstream: UpstreamClient,
        slice_size: u32,
        readahead: usize,
    ) -> Self {
        Self {
            store,
            index,
            upstream,
            slice_size,
            readahead: readahead.max(1),
            metadata: Mutex::new(HashMap::new()),
        }
    }

    fn cell(&self, object: ObjectId) -> Arc<tokio::sync::OnceCell<ObjectMeta>> {
        // The lock is held only to clone an Arc, never across an await.
        let mut map = self.metadata.lock().expect("metadata mutex poisoned");
        map.entry(object)
            .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new()))
            .clone()
    }

    /// Learn an object's length and validators, probing upstream if necessary.
    ///
    /// The index is consulted first, but it is not authoritative: if it holds an entry, we use it,
    /// and if a slice later contradicts it the slice wins (TASK-11).
    async fn metadata(
        &self,
        key: &CacheKey,
        object: ObjectId,
        url: &str,
        headers: &HeaderMap,
        probe_index: u32,
    ) -> Result<(ObjectMeta, bool), OrchestratorError> {
        if let Ok(Some(meta)) = self.index.get(&object) {
            let _ = self.index.touch(&object);
            return Ok((meta, false));
        }
        let cell = self.cell(object);
        let probed_here = !cell.initialized();
        let meta = cell
            .get_or_try_init(|| self.probe(key, object, url, headers, probe_index))
            .await?
            .clone();
        Ok((meta, probed_here))
    }

    /// Fetch one slice to learn the object's shape.
    async fn probe(
        &self,
        key: &CacheKey,
        object: ObjectId,
        url: &str,
        headers: &HeaderMap,
        index: u32,
    ) -> Result<ObjectMeta, OrchestratorError> {
        let start = index as u64 * self.slice_size as u64;
        let end = start + self.slice_size as u64 - 1;
        let response = self.upstream.fetch_range(url, headers, start, end).await?;

        let etag = response.header("etag").map(str::to_owned);
        let last_modified = response.header("last-modified").map(str::to_owned);
        let content_type = response.header("content-type").map(str::to_owned);

        let (total_len, no_ranges) = if response.status == StatusCode::PARTIAL_CONTENT {
            let total =
                response
                    .content_range_total()
                    .ok_or_else(|| OrchestratorError::NoLength {
                        url: url.to_owned(),
                    })?;
            (total, false)
        } else {
            // The origin ignored Range. Full handling is TASK-16; the length is still usable.
            (response.body.len() as u64, true)
        };

        let now = now_secs();
        let meta = ObjectMeta {
            key: key.key.clone(),
            total_len,
            generation: 0,
            etag,
            last_modified,
            content_type,
            no_ranges,
            created: now,
            last_seen: now,
        };

        if !no_ranges {
            // The probe already paid for these bytes; keep them rather than fetching again.
            let value = SliceValue::new(header_for(&meta, self.slice_size), response.body);
            self.store.insert(SliceKey::new(object, 0, index), value);
        }
        let _ = self.index.put(&object, &meta);
        Ok(meta)
    }

    /// Decide what to send, without producing any body.
    #[allow(clippy::too_many_arguments)]
    pub async fn plan(
        &self,
        key: &CacheKey,
        url: &str,
        headers: &HeaderMap,
        raw_range: Option<&str>,
    ) -> Result<Plan, OrchestratorError> {
        let object = key.object_id();

        // Parse before probing: a multi-range or unparseable header means serve the whole object,
        // which changes which slice is worth probing with.
        let spec = match raw_range.map(range::parse_range) {
            None => None,
            Some(Ok(spec)) => Some(spec),
            Some(Err(_)) => None,
        };
        let probe_index = match spec {
            // A suffix range cannot be resolved without the length, so probe slice 0.
            Some(range::RangeSpec::Suffix(_)) | None => 0,
            Some(range::RangeSpec::FromTo(start, _)) | Some(range::RangeSpec::From(start)) => {
                (start / self.slice_size as u64) as u32
            }
        };

        let (meta, probed_here) = self
            .metadata(key, object, url, headers, probe_index)
            .await?;

        if meta.no_ranges {
            return Err(OrchestratorError::NoRangesUnsupported);
        }

        let wanted = match spec {
            None => match range::whole(meta.total_len) {
                Some(r) => r,
                None => ByteRange { start: 0, end: 0 },
            },
            Some(spec) => range::resolve(spec, meta.total_len).map_err(|_| {
                OrchestratorError::Unsatisfiable {
                    total_len: meta.total_len,
                }
            })?,
        };
        let partial = spec.is_some();

        let cache_status = if meta.total_len == 0 {
            CacheStatus::Hit
        } else {
            let plan = range::plan(wanted, self.slice_size);
            let mut resident = plan
                .indices()
                .filter(|i| {
                    self.store
                        .contains(&SliceKey::new(object, meta.generation, *i))
                })
                .count() as u32;
            // The probe fetched a slice moments ago on this request's behalf; it is not evidence
            // of a prior request, so a cold request reports MISS rather than PARTIAL.
            if probed_here && plan.first <= probe_index && probe_index <= plan.last {
                resident = resident.saturating_sub(1);
            }
            CacheStatus::classify(resident, plan.count())
        };

        Ok(Plan {
            status: if partial {
                StatusCode::PARTIAL_CONTENT
            } else {
                StatusCode::OK
            },
            wanted,
            total_len: meta.total_len,
            cache_status,
            meta,
            object,
            partial,
        })
    }

    /// Fetch one slice, coalescing with any concurrent request for it.
    ///
    /// Spawned as a detached task by the body stream, so a client disconnect does not cancel it
    /// (FR-31).
    pub async fn slice(
        self: Arc<Self>,
        plan: Plan,
        url: String,
        headers: HeaderMap,
        index: u32,
    ) -> Result<SliceValue, OrchestratorError> {
        let key = SliceKey::new(plan.object, plan.meta.generation, index);
        let extent = range::slice_extent(index, self.slice_size, plan.total_len);
        let upstream = self.upstream.clone();
        let header = header_for(&plan.meta, self.slice_size);
        let expected_etag = plan.meta.etag.clone();
        let expected_len = extent.len();
        let fetch_url = url.clone();

        self.store
            .get_or_fetch(key, move || async move {
                let response = upstream
                    .fetch_range(&fetch_url, &headers, extent.start, extent.end)
                    .await?;
                let etag = response.header("etag").map(str::to_owned);
                if etag != expected_etag {
                    // The object changed under us. There is no correct way to finish a response
                    // whose first half came from a version that no longer exists, so fail and let
                    // TASK-17 turn this into a generation bump.
                    anyhow::bail!("validators changed mid-object ({expected_etag:?} -> {etag:?})");
                }
                if response.body.len() as u64 != expected_len {
                    anyhow::bail!(
                        "slice {index}: expected {expected_len} bytes, upstream sent {}",
                        response.body.len()
                    );
                }
                Ok(SliceValue::new(header, response.body))
            })
            .await
            .map_err(OrchestratorError::from)
    }

    /// The slice indices a plan needs, in order.
    pub fn indices(&self, plan: &Plan) -> Vec<u32> {
        if plan.total_len == 0 {
            return Vec::new();
        }
        range::plan(plan.wanted, self.slice_size)
            .indices()
            .collect()
    }

    /// The window of a slice's payload that satisfies the request.
    pub fn window(&self, plan: &Plan, index: u32) -> (usize, usize) {
        range::payload_window(index, self.slice_size, plan.wanted)
    }

    pub fn readahead(&self) -> usize {
        self.readahead
    }

    pub fn slice_size(&self) -> u32 {
        self.slice_size
    }

    pub fn store(&self) -> &SliceStore {
        &self.store
    }
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

/// Extract the bytes of `value` that satisfy `window`, or explain why it cannot.
pub fn payload_window(value: &SliceValue, window: (usize, usize)) -> Result<Bytes, String> {
    let (from, to) = window;
    if from > to || to > value.payload.len() {
        return Err(format!(
            "slice is {} bytes; wanted window {from}..{to}",
            value.payload.len()
        ));
    }
    Ok(value.payload.slice(from..to))
}
