# TASK-16: no_ranges path and object-level filler

## Context
Milestone: M2 | Requirements: FR-13, FR-32

Some upstreams ignore `Range` and return the whole object. Without this path, one client asking
for 1 MiB of a 60 GB object pulls 60 GB, and thirty clients pull it thirty times.

## Implementation Plan
### Phase 1: Filler registry
- [ ] `DashMap<object_id, Arc<FillState>>`
- [ ] One task streams the full body, cuts slices into the store, publishes per-slice readiness
- [ ] Other requests subscribe to readiness rather than issuing their own fetch (FR-32)

### Phase 2: Serving from a fill in progress
- [ ] A request for slice `i` waits on readiness for `i` only, not the whole object
- [ ] Requests for already-landed slices are served immediately from the store

### Phase 3: Lifecycle
- [ ] Fill completes even if every subscriber disconnects (FR-31)
- [ ] Fill failure wakes subscribers with an error rather than hanging them
- [ ] Registry entry removed on completion without racing a new subscriber

## Technical Decisions
- Object-level single-flight is a different mechanism from slice-level single-flight, not a
  special case of it. The store cannot dedup what it cannot key.
- `no_ranges` is remembered per object in the index so the second request skips the probe.

## Dependencies
- Requires: TASK-12
- Blocks: TASK-20 (chaos exercises this path)

## Completion Checklist
- [ ] N clients on a range-ignoring origin produce exactly one upstream stream
- [ ] Subscriber waiting on a late slice is woken as it lands, not at object completion
- [ ] All subscribers disconnecting still completes the fill
- [ ] Fill failure propagates to every subscriber
