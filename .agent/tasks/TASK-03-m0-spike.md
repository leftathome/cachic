# TASK-03: M0 spike - hyper + reqwest + foyer sliced GET prototype

## Context
Milestone: M0 | Requirements: validates FR-10, FR-11, FR-12, FR-30, FR-40 before committing to them

The plan's central bet is that foyer removes the need to write a cache engine. This throwaway
prototype exists to falsify that bet cheaply. It is explicitly not production code and is not
expected to survive into M1 - what survives is the measurements and the ADRs.

## Implementation Plan
### Phase 1: Mock upstream
- [ ] `mockcdn` serving deterministic content `f(url, offset)` so any byte can be verified
      without storing a reference copy
- [ ] Range-capable mode (`206` with `Content-Range`, `ETag`)
- [ ] Range-ignoring mode (always `200`) to exercise the `no_ranges` shape
- [ ] Configurable delay/bandwidth to emulate a WAN origin

### Phase 2: Store wrapper
- [ ] foyer `HybridCache<SliceKey, Vec<u8>>` with memory + disk tiers
- [ ] Slice codec: header (magic, slice_size, total_len, etag, generation, xxh3) + payload
- [ ] Verify checksum on read

### Phase 3: Proxy path
- [ ] hyper server accepting any `Host`
- [ ] Parse `Range`, compute slice indices, probe for total length via the first slice
- [ ] Ordered pipeline over `store.fetch(key, || upstream_fetch_slice(..))` with a bounded window
- [ ] Correct `200`/`206`, `Content-Length`, `Content-Range`, `X-Cache`

### Phase 4: Verification
- [ ] Differential check: bytes through the proxy equal bytes from `mockcdn`, for random URLs and
      random ranges, cold and warm
- [ ] Concurrency check: N clients requesting the same cold object cause approximately one
      upstream fetch per slice (proves foyer's `fetch` coalescing, FR-30)

## Technical Decisions
- Prototype lives in its own path (`crates/cachic/src/bin/spike.rs` or a `spike/` crate) and is
  deleted or rewritten in M1. Do not let it accrete features.
- `Vec<u8>` values are acceptable here; the zero-copy `Bytes` question is an M1 concern.
- Single slice size (1 MiB) only; per-service overrides are out of scope for the spike.

## Dependencies
- Requires: TASK-01
- Blocks: TASK-04, TASK-05, TASK-06

## Completion Checklist
- [ ] Differential test passes cold and warm
- [ ] Coalescing verified by counting upstream requests
- [ ] Range-ignoring upstream handled (or the gap explicitly documented for M2)
- [ ] Findings written up for the ADRs
