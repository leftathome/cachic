# 0003. Store engine and object index

- **Status**: **Provisional.** The chosen engine has a defect that blocks a P0 requirement.
- **Date**: 2026-09-01
- **Context**: M0 (TASK-03, TASK-04), plan section 1.3, plan section 10

## Context

The plan's central bet: foyer removes the need to write a cache engine, which is the single
largest piece of work in the project (costed at 3-5 weeks plus ongoing tuning). The store must
satisfy FR-40 through FR-45, of which FR-43 - "serve hits within seconds after restart; full index
rebuilt in the background; no dependency on a clean shutdown" - is P0.

The plan already named this as the top engineering risk and prescribed the mitigation we followed:
pin the version, keep it behind a `store::Store` trait boundary, measure in M0, and keep a
fallback design ready (per-object sparse files plus a bitmap sidecar, about two weeks).

## Decision

Keep foyer behind the `Store` trait, but **do not commit to it**. M0 does not confirm the bet.

The design decisions that do not depend on the engine are confirmed and stand:

- **Slices are self-describing** (magic, slice size, total length, validators, generation, xxh3
  over the payload). This is FR-44, and it is what makes the object index a rebuildable
  acceleration structure rather than the source of truth. Verified by round-trip property tests,
  and by tests proving corrupt, truncated and foreign bytes all fail to decode rather than being
  served.
- **`redb` for the object index**, unchanged. M0 did not exercise it; the spike used an in-memory
  map, and the index is TASK-11's work.

## The defect

After a clean close and reopen, foyer 0.22.4 returns fewer than half the entries written, with no
error reported. Measured at 5.9%, 27.7%, 29.3%, 34.8%, 46.9% and exactly 50.0% across
configurations. Full data and methodology in `docs/benchmarks/m0/README.md`.

This was checked against the hypothesis that we were using foyer incorrectly. The `foyerprobe`
binary reproduces it through foyer's public API with plain `u64` keys and `Vec<u8>` values, with
no cachic types involved, populating via `insert` (not just `get_or_fetch`), under both cache
policies, with `flush_on_close(true)`, buffered IO, and a disk tier four times the data written so
nothing is evicted for capacity. It is not our codec, our wrapper, our fill path, the policy, the
IO mode, or the capacity.

A second defect: `HybridCache::close()` does not return within 20 seconds after a read pass
containing failed fetch closures, which threatens FR-62.

Against that, what foyer did do well:

- **Request coalescing works exactly as FR-30 needs.** 32 concurrent misses on one key produce one
  fetch; end to end, 24 clients on a cold 8-slice object produced at most 12 upstream requests, and
  a 256 MiB object at 1 MiB slices produced exactly 257 upstream requests (256 slices plus a probe).
  This is the behaviour we wanted over nginx's `proxy_cache_lock`, and it is not trivial to build.
- **Warm memory-tier reads are effectively free** (25 GiB/s including a checksum over every byte).
- **Memory accounting is honest**: RSS tracked the configured tier in every run.

## Cost of the index

foyer costs 381-463 bytes of RSS per indexed entry. lancache's published rule of thumb - 1 MB of
shared memory per ~8 GB of 1 MiB slices - is about 128 bytes. We are roughly three times heavier
than the incumbent, which is a sizing-documentation obligation (a 2 TB cache is ~760 MB of index,
10 TB is ~3.8 GB) and an argument for a larger default slice size on large caches. This is
independent of the recovery defect and applies to any decision to keep foyer.

## Options

1. **File upstream and wait.** Cheapest if foyer fixes it. Unknown latency, and M1's store task is
   blocked meanwhile.
2. **File upstream and carry a patch.** Viable only once the root cause is understood, which
   requires reading foyer's block engine and recovery scanner.
3. **Fall back to the plan's own design**: per-object sparse files plus a bitmap sidecar, redb for
   the index. Costed at about two weeks. The `Store` trait boundary means nothing above the store
   changes, which is exactly why that boundary was drawn.

The self-describing slice format means option 3 is not a rewrite of the interesting logic. It also
means recovery is tractable by construction: the slices on disk carry everything needed.

## Next action

File both defects upstream with `foyerprobe` as the reproducer. Decide between the options on the
upstream response. Do not start TASK-11 until this is resolved - it is the task that would have to
be redone.

## What would overturn this

A foyer release, or a configuration we have not found, that recovers 100% of written entries
across a clean restart and does not hang on close. That would move this ADR to Accepted unchanged.
