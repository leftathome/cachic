# TASK-18: Disconnect semantics, graceful shutdown, limits

## Context
Milestone: M2 | Requirements: FR-09, FR-31, FR-62, NFR-4

Operational behaviour under stress and during restarts. A cache that loses in-flight work on every
deploy is a cache that never warms up.

## Implementation Plan
### Phase 1: Disconnect
- [ ] Fills are tasks owned by the store, never attached to connection-scoped cancellation
- [ ] Configurable, defaulting to "complete the fill"
- [ ] Explicit test that a disconnect mid-fill still stores the slice

### Phase 2: Graceful shutdown
- [ ] Stop accepting, finish in-flight slices, flush, exit within a bounded time
- [ ] `/readyz` reports not-ready as soon as draining starts
- [ ] Bounded: a hung upstream must not prevent exit

### Phase 3: Limits
- [ ] Per-service and global upstream concurrency limits with backpressure
- [ ] 10 000 open client connections and 500 in-flight upstream fetches (NFR-4)
- [ ] Limits shed or queue predictably; document which

## Technical Decisions
- Shutdown is bounded because Kubernetes will SIGKILL us anyway; better to flush what we can and
  exit deliberately.
- Backpressure propagates to the client rather than growing an unbounded queue.

## Dependencies
- Requires: TASK-12, TASK-13
- Blocks: TASK-20, TASK-22

## Completion Checklist
- [ ] Disconnect-mid-fill test passes
- [ ] Shutdown completes within the bound with a hung upstream
- [ ] Connection and fetch limits verified at NFR-4 scale
- [ ] `/readyz` flips at the right moment for a rolling replacement
