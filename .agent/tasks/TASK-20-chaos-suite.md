# TASK-20: Chaos suite

## Context
Milestone: M2 | Requirements: FR-42, FR-43, NFR-7; M2 exit criteria

The cache runs unattended on someone's LAN, gets power-cycled, and fills its disk. "Zero corrupt
bytes served" (NFR-7) is only a claim until something has tried hard to break it.

## Implementation Plan
### Phase 1: Scenarios
- [ ] `kill -9` mid-fill, repeatedly, at randomised offsets
- [ ] Disk full (small tmpfs) during a fill and during eviction
- [ ] Slow disk via cgroup IO throttling
- [ ] Flaky upstream: 5xx, connection reset, stall mid-body
- [ ] DNS failure and DNS returning the cache's own address
- [ ] Sustained fill at 1, 2.5, 5 and 10 Gbit rates, asserting no slices are silently dropped
      (`storage_queue_channel_overflow` stays at zero)

### Phase 2: Assertions
- [ ] After every scenario: no partial slice readable, no corrupt byte served
- [ ] Recovery: serving hits within seconds, index rebuilt in the background
- [ ] Metrics reflect what happened (checksum failures, evictions, upstream errors)

### Phase 3: Harness
- [ ] compose profile `chaos`, scripted, runnable locally and nightly in CI

## Technical Decisions
- Runs in containers because that is the shipped artefact and because cgroup throttling and disk
  limits are natural there.
- Every scenario asserts on metrics as well as bytes. Silent recovery that loses the whole cache is
  still a failure.

## Dependencies
- Requires: TASK-14, TASK-16, TASK-17, TASK-18
- Blocks: M2 exit criteria

## Completion Checklist
- [ ] All scenarios green
- [ ] Suite runs nightly in CI
- [ ] Each failure mode produces an actionable log line
- [ ] 48-hour homelab soak clean (M2 exit criterion)
