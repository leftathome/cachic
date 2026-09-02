# 0001. Language and runtime

- **Status**: Accepted
- **Date**: 2026-09-01
- **Context**: M0 (TASK-03, TASK-04)

## Context

The implementation plan bets on Rust with tokio, and the tie-breaker in that argument was reuse:
"the PRD's reuse-community-code goal is best met by not writing a cache engine." Go was costed in
Appendix A at +3-5 weeks for a custom store and -1-2 weeks elsewhere.

M0 built a working prototype (TASK-03) and measured it (TASK-04).

## Decision

Continue in Rust with tokio and hyper.

## Consequences

What M0 demonstrated:

- The whole slice-aware path - probe, plan, ordered pipeline with a bounded read-ahead window,
  coalesced fetches, correct 200/206/416 - is about 700 lines of readable Rust, verified by a
  differential test over random ranges.
- Per-connection memory falls out of the design (`readahead * slice_size`) rather than needing a
  separate limiter, and measured RSS tracked the configured memory tier.
- The slice codec, including a checksum over every byte, runs at 4.4 GB/s encode and 12 GB/s
  decode. Nothing in our own code is near being a bottleneck.
- End-to-end throughput was bounded by the HTTP and copy path, not by the store.

The reuse argument holds. foyer covers the store, its coalescing satisfies FR-30 end to end, and
ADR 0003 is accepted. M0 briefly appeared to falsify this; that was a benchmark error, recorded in
ADR 0003. The language decision does not rest on reuse alone either - no GC pauses on GB/s of 1 MiB
buffers, `Bytes` for zero-copy fan-out and static musl binaries are what carried it.

The contributor-pool concern from the plan stands and is not answered by M0.

## What would overturn this

If we ever had to write a store from scratch, the store cost would become common to both
languages, and Go's larger contributor pool in the lancache community plus `sendfile` on disk-tier
hits would become live arguments again. ADR 0003 is accepted, so that condition does not hold.

If it ever does, it must not be acted on from the armchair: plan section 0.1's claim that Go has no
equivalent library is inherited and unverified, and no survey of either ecosystem has been done.
Reopening this ADR requires that survey first, not a preference.
