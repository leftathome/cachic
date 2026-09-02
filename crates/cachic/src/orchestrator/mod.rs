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

pub mod filler;
pub mod readahead;
pub mod validators;

use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc, Mutex},
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
    #[error("filling {url}: {reason}")]
    Fill { url: String, reason: String },
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
    /// Object-level single-flight for origins that ignore `Range` (FR-32).
    fills: filler::FillRegistry,
    /// Speculative prefetch policy (FR-16). Only fires for clients that are clearly streaming.
    readahead_policy: readahead::ReadaheadPolicy,
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
            fills: filler::FillRegistry::new(),
            readahead_policy: readahead::ReadaheadPolicy::new(readahead.max(1)),
        }
    }

    /// Invalidate an object because its validators changed (FR-14).
    ///
    /// Generation is part of the slice key, so incrementing it makes every stored slice of the
    /// old version unreachable at once. There is no sweep and no window in which a response
    /// could mix versions; the old slices are evicted in the ordinary course of things.
    fn bump_generation(&self, object: ObjectId) -> u32 {
        let next = match self.index.invalidate(&object) {
            Ok(generation) => generation,
            Err(e) => {
                // If the index cannot record the invalidation, the old version stays addressable
                // and a later request could serve it. Say so loudly rather than continuing with a
                // guessed generation.
                tracing::error!(
                    object = %hex_id(&object),
                    error = %e,
                    "could not invalidate the object index; stale content may be served"
                );
                1
            }
        };
        // Drop the cached metadata so the next request re-probes and learns the new validators
        // rather than serving from the version that just went away.
        self.metadata
            .lock()
            .expect("metadata mutex poisoned")
            .remove(&object);
        next
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
        // A stale entry means the validators changed: everything it records except the
        // generation is unreliable, so re-probe rather than serving the old version's shape.
        match self.index.get(&object) {
            Ok(Some(meta)) if !meta.stale => {
                let _ = self.index.touch(&object);
                return Ok((meta, false));
            }
            _ => {}
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

        // A previous version's generation survives in the index even when the entry is stale,
        // and must be respected or the old slices become addressable again.
        let generation = match self.index.get(&object) {
            Ok(Some(previous)) => previous.generation,
            _ => 0,
        };
        let now = now_secs();
        let meta = ObjectMeta {
            key: key.key.clone(),
            total_len,
            generation,
            etag,
            last_modified,
            content_type,
            no_ranges,
            created: now,
            last_seen: now,
            stale: false,
        };

        if !no_ranges {
            // The probe already paid for these bytes; keep them rather than fetching again.
            let value = SliceValue::new(header_for(&meta, self.slice_size), response.body);
            self.store
                .insert(SliceKey::new(object, generation, index), value);
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

        // Parse before probing: a multi-range or unparsable header means serve the whole object,
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
        // A range-ignoring origin cannot serve one slice, so the whole object is filled once and
        // this request waits only for the slice it needs (FR-13, FR-32).
        if plan.meta.no_ranges {
            self.ensure_filled(&plan, &url, &headers, index).await?;
            let key = SliceKey::new(plan.object, plan.meta.generation, index);
            return match self.store.get(&key).await? {
                Some(value) => Ok(value),
                None => Err(OrchestratorError::Fill {
                    url,
                    reason: format!("slice {index} was reported ready but is not in the store"),
                }),
            };
        }

        let key = SliceKey::new(plan.object, plan.meta.generation, index);
        let extent = range::slice_extent(index, self.slice_size, plan.total_len);
        let upstream = self.upstream.clone();
        let header = header_for(&plan.meta, self.slice_size);
        let expected_validators =
            validators::Validators::new(plan.meta.etag.clone(), plan.meta.last_modified.clone());
        let plan_object = plan.object;
        let url_for_error = url.clone();
        let expected_etag_for_error = plan.meta.etag.clone();
        let mismatch = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed: Arc<Mutex<Option<validators::Validators>>> = Arc::new(Mutex::new(None));
        let mismatch_flag = mismatch.clone();
        let observed_slot = observed.clone();
        let expected_len = extent.len();
        let fetch_url = url.clone();

        self.store
            .get_or_fetch(key, move || async move {
                let mismatch = mismatch_flag;
                let observed = observed_slot;
                let response = upstream
                    .fetch_range(&fetch_url, &headers, extent.start, extent.end)
                    .await?;
                let found = validators::Validators::new(
                    response.header("etag").map(str::to_owned),
                    response.header("last-modified").map(str::to_owned),
                );
                if !expected_validators.matches(&found) {
                    // The object changed under us. There is no correct way to finish a response
                    // whose first half came from a version that no longer exists, so this fails
                    // and the caller bumps the generation.
                    //
                    // The signal is a shared flag rather than a marker in the error message: the
                    // error crosses the store's boundary and is rewrapped by foyer, so its text
                    // does not survive. Relying on the message meant the bump silently never
                    // happened.
                    mismatch.store(true, Ordering::Relaxed);
                    observed
                        .lock()
                        .expect("validator mutex poisoned")
                        .replace(found.clone());
                    anyhow::bail!(
                        "validators changed mid-object: {expected_validators:?} -> {found:?}"
                    );
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
            .map_err(|e| {
                // A validator change is not an ordinary fetch failure: the object being assembled
                // no longer exists. Invalidate it so the next request sees the new version, and
                // return a distinct error so the response aborts rather than completing with a
                // mixture.
                //
                // The signal is a shared flag rather than a marker in the error text: the error
                // crosses the store boundary and is rewrapped, so the message does not survive.
                if mismatch.load(Ordering::Relaxed) {
                    let generation = self.bump_generation(plan_object);
                    let now = observed
                        .lock()
                        .ok()
                        .and_then(|v| v.clone())
                        .and_then(|v| v.etag);
                    tracing::warn!(
                        object = %hex_id(&plan_object),
                        generation,
                        "validators changed mid-object; invalidated"
                    );
                    return OrchestratorError::ValidatorChanged {
                        url: url_for_error,
                        had: expected_etag_for_error,
                        now,
                    };
                }
                OrchestratorError::from(e)
            })
    }

    /// Ensure slice `index` of a `no_ranges` object is readable, filling the object if nobody
    /// else is (FR-13, FR-32).
    ///
    /// Exactly one caller streams the object and cuts it into slices, publishing readiness as
    /// each lands. Everyone else waits on the slice they need, not on completion: on a 60 GB
    /// object over a WAN link that difference is hours.
    pub async fn ensure_filled(
        self: &Arc<Self>,
        plan: &Plan,
        url: &str,
        headers: &HeaderMap,
        index: u32,
    ) -> Result<(), OrchestratorError> {
        // Already stored: nothing to wait for.
        if self
            .store
            .contains(&SliceKey::new(plan.object, plan.meta.generation, index))
        {
            return Ok(());
        }

        match self.fills.claim(plan.object) {
            filler::Role::Subscriber(rx) => {
                filler::wait_for(rx, index)
                    .await
                    .map_err(|reason| OrchestratorError::Fill {
                        url: url.to_owned(),
                        reason,
                    })
            }
            filler::Role::Filler(fill) => {
                let result = self.run_fill(plan, url, headers, &fill).await;
                // The registry entry is released whatever happens. Leaving it would make the
                // object permanently unfillable after one failure.
                self.fills.release(&plan.object);
                match result {
                    Ok(count) => {
                        fill.complete(count);
                        if index < count {
                            Ok(())
                        } else {
                            Err(OrchestratorError::Fill {
                                url: url.to_owned(),
                                reason: format!(
                                    "fill produced {count} slices; slice {index} was never reached"
                                ),
                            })
                        }
                    }
                    Err(e) => {
                        // Subscribers must be woken with the reason rather than left waiting.
                        fill.fail(e.to_string());
                        Err(e)
                    }
                }
            }
        }
    }

    /// Stream a whole object, cutting it into slices as it arrives.
    ///
    /// Memory is bounded to one slice plus whatever the HTTP client has buffered, which is the
    /// point: these are the objects measured in tens of gigabytes.
    async fn run_fill(
        &self,
        plan: &Plan,
        url: &str,
        headers: &HeaderMap,
        fill: &Arc<filler::Fill>,
    ) -> Result<u32, OrchestratorError> {
        use futures_util::StreamExt;

        let (_headers, stream) = self.upstream.fetch_stream(url, headers).await?;
        futures_util::pin_mut!(stream);

        let slice_size = self.slice_size as usize;
        let header = header_for(&plan.meta, self.slice_size);
        let mut buffer = bytes::BytesMut::with_capacity(slice_size);
        let mut index = 0u32;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| {
                OrchestratorError::Upstream(crate::upstream::client::UpstreamError::Request {
                    url: url.to_owned(),
                    source,
                })
            })?;
            buffer.extend_from_slice(&chunk);
            while buffer.len() >= slice_size {
                let payload = buffer.split_to(slice_size).freeze();
                self.store.insert(
                    SliceKey::new(plan.object, plan.meta.generation, index),
                    SliceValue::new(header.clone(), payload),
                );
                index += 1;
                // Publish as each slice lands, so a waiter for slice 3 wakes at slice 3 rather
                // than at the end of a 60 GB object.
                fill.publish(index);
            }
        }
        if !buffer.is_empty() {
            self.store.insert(
                SliceKey::new(plan.object, plan.meta.generation, index),
                SliceValue::new(header, buffer.freeze()),
            );
            index += 1;
            fill.publish(index);
        }

        Ok(index)
    }

    /// Speculatively fetch slices beyond a request, if the client is streaming (FR-16).
    ///
    /// Detached tasks: prefetch must never delay the response it is speculating on behalf of.
    /// Slices already resident are skipped, so a warm object costs nothing.
    ///
    /// This is the only place cachic fetches bytes nobody asked for, which is why it is gated on
    /// a demonstrated pattern rather than run for every request. Benchmark S4 measures upstream
    /// amplification at exactly 1.00; a prefetch policy that worsens that for random-access
    /// clients is not worth its throughput.
    pub fn maybe_prefetch(self: &Arc<Self>, plan: &Plan, url: &str, headers: &HeaderMap) {
        if plan.total_len == 0 || plan.meta.no_ranges {
            return;
        }
        let span = range::plan(plan.wanted, self.slice_size);
        let access = self
            .readahead_policy
            .observe(plan.object, span.first, span.last);
        let last_slice =
            ((plan.total_len - 1) / self.slice_size as u64).min(u32::MAX as u64) as u32;

        for index in self
            .readahead_policy
            .prefetch(access, span.last, last_slice)
        {
            if self
                .store
                .contains(&SliceKey::new(plan.object, plan.meta.generation, index))
            {
                continue;
            }
            let orchestrator = self.clone();
            let plan = plan.clone();
            let url = url.to_owned();
            let headers = headers.clone();
            tokio::spawn(async move {
                // Failures here are silent by design: nobody is waiting on a prefetch, and a
                // speculative fetch that fails costs only the speculation.
                let _ = orchestrator.slice(plan, url, headers, index).await;
            });
        }
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

fn hex_id(id: &ObjectId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
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
