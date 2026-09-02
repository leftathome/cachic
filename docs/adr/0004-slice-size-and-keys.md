# 0004. Slice size, key scheme, generation semantics

- **Status**: Accepted
- **Date**: 2026-09-01
- **Context**: M0 (TASK-03), plan section 1.3

## Decision

- **Slice size**: 1 MiB default, configurable, overridable per service. Persisted with the cache;
  a change is detected at startup and refused unless explicitly forced (FR-10, matching
  monolithic's `CONFIGHASH` behaviour).
- **Object id**: `blake3(identifier || normalised_key)[..16]`. 16 bytes is ample against collision
  at the scales involved and keeps the slice key small.
- **Slice key**: `(object_id: [u8;16], generation: u32, index: u32)`, 24 bytes encoded, fixed
  width, hand-encoded rather than serde.
- **Generation** is part of the key. A validator change bumps the generation, which makes
  invalidation atomic by construction: old-generation slices become unreachable immediately and
  are evicted normally. No sweep, no tombstones, no window in which a response could mix two
  versions.

## Consequences

Validated in M0 by tests: distinct generations address distinct slices, and the key round-trips
through its encoding.

The index-memory measurement (ADR 0003) gives slice size a second dimension: per-entry index cost
is roughly 400 bytes regardless of slice size, so on very large caches a bigger slice reduces index
RAM proportionally. A 2 TB cache is ~760 MB of index at 1 MiB slices and ~190 MB at 4 MiB. Against
that, a larger slice wastes more upstream bandwidth on small random ranges (FR-16, Windows Update
and Blizzard workloads). The default stays at 1 MiB to match monolithic; the sizing guide should
show the trade rather than hiding it.

Hand-encoding the key rather than deriving serde is deliberate: it is a hot, fixed-width structure,
and `estimated_size` must match the encoded length exactly or the store's capacity accounting is
silently wrong. There is a test asserting that equality.

## What would overturn this

Measured collision behaviour at 16 bytes (it will not), or benchmark evidence from S5 (random
64 KiB-8 MiB ranges) that 1 MiB is the wrong default for range-heavy services.
