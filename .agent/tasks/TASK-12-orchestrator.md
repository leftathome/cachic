# TASK-12: Orchestrator - probe, slice plan, pipeline

## Context
Milestone: M1 | Requirements: FR-11, FR-12, FR-16, FR-30, FR-31

Where a client request becomes a set of slice fetches. Everything the product claims over nginx -
coalescing that streams rather than blocks, read-ahead, bounded memory - lives here.

## Implementation Plan
### Phase 1: Probe
- [ ] Unknown object -> fetch the first needed slice with `Range: bytes=a-b`
- [ ] `206` yields total length and validators from `Content-Range`/`ETag`/`Last-Modified`
- [ ] `200` marks the object `no_ranges` (full handling in TASK-16)

### Phase 2: Slice plan
- [ ] Map the requested byte range to slice indices `[i0..=i1]`
- [ ] Compute response headers before the body starts
- [ ] Arithmetic property-tested at object boundaries, single-byte ranges, last partial slice

### Phase 3: Pipeline
- [ ] `store.fetch((object_id, gen, i), || upstream.fetch_slice(..))` per slice
- [ ] Bounded `READAHEAD_SLICES` window; futures awaited in order
- [ ] Write each slice's relevant byte range to the body in order

### Phase 4: Semantics
- [ ] Client disconnect does not cancel an in-flight fill (FR-31)
- [ ] Cache status classification: `HIT` all from store, `MISS` none, `PARTIAL` mixed

## Technical Decisions
- Backpressure is the window, not a semaphore bolted on later. Per-connection RAM is
  `READAHEAD_SLICES * slice_size` by construction.
- Coalescing is foyer's `fetch`, giving one upstream request and N streaming readers. Do not
  reintroduce a lock-and-wait path.

## Dependencies
- Requires: TASK-08, TASK-09, TASK-10, TASK-11
- Blocks: TASK-16, TASK-17

## Completion Checklist
- [ ] Differential test passes cold and warm for random ranges
- [ ] Coalescing verified by upstream request count under N concurrent clients
- [ ] Disconnect test: fill completes and is stored after the client hangs up
- [ ] RSS under load matches the window calculation
