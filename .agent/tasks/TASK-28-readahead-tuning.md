# TASK-28: Read-ahead tuning

## Context
Milestone: M4 | Requirements: FR-16, NFR-1, NFR-5

Read-ahead is the difference between line rate and stutter on cold content, and the wrong window
is the difference between bounded RSS and an OOM.

## Implementation Plan
- [ ] Detect sequential streaming versus random-range access per connection
- [ ] Prefetch the next N slices on sequential access; do not prefetch on random access
- [ ] Tune the default window against the benchmark scenarios, especially S1, S4 and S5
- [ ] Verify RSS stays within `READAHEAD_SLICES * slice_size` per connection at NFR-4 concurrency
- [ ] Document the memory arithmetic in the config reference so operators can size it

## Technical Decisions
- Random-range clients (Windows Update, Blizzard) must not trigger read-ahead; prefetching there
  is pure upstream amplification.
- The default is chosen from measurements, not intuition, and the measurement is committed.

## Dependencies
- Requires: TASK-12, TASK-25
- Blocks: v1.0

## Completion Checklist
- [ ] Sequential detection tested against both client shapes
- [ ] Default window justified by benchmark data
- [ ] RSS bound verified at 10 000 connections
- [ ] Upstream amplification unchanged for random-range workloads
