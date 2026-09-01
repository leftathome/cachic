# TASK-17: Generations, stale-on-error, If-Range

## Context
Milestone: M2 | Requirements: FR-14, FR-17, FR-22

Content that is "effectively immutable" is not actually immutable. When an object changes under
us mid-stream, the only unacceptable outcome is serving a mix of two versions.

## Implementation Plan
### Phase 1: Generation bump
- [ ] Compare each slice response's validators against the object's current generation
- [ ] On mismatch: bump generation, abort the client stream (connection close makes the client
      retry against the new version), log and count
- [ ] Old-generation slices become unreachable and are evicted normally

### Phase 2: Stale-on-error
- [ ] Never cache 3xx/4xx/5xx
- [ ] On upstream 5xx or timeout, serve the slices we have and fail only the missing ones

### Phase 3: If-Range
- [ ] Match -> `206`; mismatch -> full `200`

## Technical Decisions
- Aborting the stream is deliberate. There is no correct way to finish a response whose first half
  came from a version that no longer exists.
- Generation is part of the slice key, so a bump is atomic by construction - no sweep required.

## Dependencies
- Requires: TASK-12
- Blocks: TASK-20

## Completion Checklist
- [ ] `mockcdn` changing validators mid-object triggers exactly one bump and one aborted stream
- [ ] No response ever mixes generations (asserted by the differ)
- [ ] Stale-on-error tested against a 5xx origin with a partially warm object
- [ ] `If-Range` tested both ways
