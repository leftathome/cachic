# TASK-19: Admin API and disk guard

## Context
Milestone: M2 | Requirements: FR-46, FR-54

nginx gave operators no way to ask the cache anything. The admin API is where "what is in there,
and can you drop it" gets an answer.

## Implementation Plan
### Phase 1: API
- [ ] `axum` on the admin port, separate from the data plane
- [ ] `GET /stats` - store size, entries, hit rates by service
- [ ] `GET /services` - loaded services and their rules
- [ ] `POST /purge` - by service or path prefix
- [ ] `POST /reload` - re-read the domain list
- [ ] `POST /drain` - begin graceful shutdown

### Phase 2: Security
- [ ] Local/cluster-only by default
- [ ] Optional bearer token; never required, never defaulted to a fixed value
- [ ] Admin port never bound to the same listener as the data plane

### Phase 3: Disk guard
- [ ] `MIN_FREE_DISK`: reduce the effective cap when the filesystem runs low
- [ ] Metric and log when the guard engages

## Technical Decisions
- Purge is by service or prefix, not by regex over every key - an unbounded scan on a 2 TB cache
  is a denial of service against ourselves.
- The API is a stability surface once documented; version it in the docs from the start.

## Dependencies
- Requires: TASK-11, TASK-13
- Blocks: TASK-24 (LANCache Manager wants purge)

## Completion Checklist
- [ ] Every endpoint tested including auth on and off
- [ ] Purge verified to remove slices and index entries together
- [ ] Guard engages on a small tmpfs and releases when space returns
- [ ] Admin port unreachable from the data-plane listener
