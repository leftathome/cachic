# 0003. Store engine and object index

- **Status**: Accepted. (Two earlier revisions marked this blocked on a store defect; that
  finding was a benchmark error and is documented below rather than removed.)
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

**Accepted. foyer stays**, behind the `store::Store` trait, with `redb` for the object index.

The design decisions that do not depend on the engine are confirmed and stand:

- **Slices are self-describing** (magic, slice size, total length, validators, generation, xxh3
  over the payload). This is FR-44, and it is what makes the object index a rebuildable
  acceleration structure rather than the source of truth. Verified by round-trip property tests,
  and by tests proving corrupt, truncated and foreign bytes all fail to decode rather than being
  served.
- **`redb` for the object index**, unchanged. M0 did not exercise it; the spike used an in-memory
  map, and the index is TASK-11's work.

## The finding that nearly reversed this, and why it was wrong

M0 initially measured foyer returning fewer than half its entries after a clean restart, and two
earlier revisions of this ADR treated that as blocking - first as a foyer defect, then as a
requirements mismatch. **Both were wrong. It was a benchmark error.**

foyer silently discards a disk write when its submit queue is saturated, incrementing
`storage_queue_channel_overflow`. For a cache that is a defensible design: a dropped write is a
future miss, not data loss. The behaviour is governed entirely by write rate:

| Insert rate | Entries kept |
|---|---|
| 2,442 MiB/s | 46.9% |
| 303 MiB/s | 100% |
| 88 MiB/s | 100% |
| 38 MiB/s | 100% |

The harness was inserting at memory speed with no backpressure - a rate cachic never produces,
since filling from a CDN over a domestic line is tens of MiB/s. It also only counted entries after
a restart, which cannot separate "dropped on write" from "not recovered"; counting live, before
the close, shows the two numbers match and recovery was never at fault. A large memory tier made
it worse by masking the drops behind RAM hits until a restart exposed them.

Full detail in `docs/benchmarks/m0/README.md`. `cargo run --release --example foyerprobe` reproduces
the whole sweep.

**This is worth recording rather than quietly deleting**, because the failure mode generalises: a
benchmark that drives a component far outside its operating range produces confident, reproducible,
completely misleading numbers, and "reproducible" felt like sufficient evidence at the time.

## Consequences

What M0 establishes about foyer, positively:

- **Request coalescing works exactly as FR-30 needs.** 32 concurrent misses on one key produce one
  fetch; end to end, 24 clients on a cold 8-slice object produced at most 12 upstream requests, and
  a 256 MiB object at 1 MiB slices produced exactly 257 upstream requests (256 slices plus a probe).
  This is the behaviour we wanted over nginx's `proxy_cache_lock`, and it is not trivial to build.
- **Warm memory-tier reads are effectively free** (25 GiB/s including a checksum over every byte).
- **Recovery is correct** when writes are not being dropped.
- **Memory accounting is honest**: RSS tracked the configured tier in every run.

Three obligations follow, and they are not optional:

1. **Expose `storage_queue_channel_overflow` and `storage_block_engine_enqueue_skip` through
   `/metrics`** (FR-50). A cache that silently declines to cache is the worst failure this product
   can have, and without these counters it is invisible. This is a TASK-13 requirement.
2. **Default above foyer's write-path settings, and document the tuning.** The success bar for
   upstream fill is 200 Mbit/s (~24 MiB/s), which foyer's defaults clear with roughly
   twenty-five times headroom, so this is not a shipping blocker. But defaults are clean only
   through 5 Gbit: at 10 Gbit (1192 MiB/s) one slice in ten is silently dropped, and fibre at that
   tier exists. Shipping 2 flushers and a 64 MiB buffer pool costs almost nothing and covers every
   tier to 10 Gbit; beyond that, operators tune. Knobs in TASK-07, worked examples in TASK-32.
   Raising `submit_queue_size_threshold` alone does nothing - the drain rate is the constraint,
   not the queue.
3. **Do not benchmark at rates the product cannot generate.** The measurement harness should pace
   writes by default.

Still open: `HybridCache::close()` did not return within 20 seconds after a read pass containing
failed fetches. That was observed before the write-rate cause was understood and may share a root
cause; it needs re-testing before it is treated as real.

## Cost of the index

foyer costs 381-463 bytes of RSS per indexed entry. lancache's published rule of thumb - 1 MB of
shared memory per ~8 GB of 1 MiB slices - is about 128 bytes. We are roughly three times heavier
than the incumbent, which is a sizing-documentation obligation (a 2 TB cache is ~760 MB of index,
10 TB is ~3.8 GB) and an argument for a larger default slice size on large caches. This is
independent of the recovery defect and applies to any decision to keep foyer.

## On alternatives

No survey of alternative stores was carried out, in Rust or in Go, and none is now needed to
unblock M1: foyer meets the requirement. Plan section 0.1's claim that Go has "no equivalent" to
foyer remains inherited and unverified, and should be treated as such if the language question is
ever genuinely reopened - but nothing in M0 reopens it. Writing a store from scratch, in either
language, is not on the table.

## Next action

Proceed to TASK-11 with foyer, carrying the three obligations above.

## What would overturn this

Evidence that the drop threshold is reachable even with the tuned settings above - a faster
upstream than 10 Gbit, sustained multi-hour fills, or heavy concurrent read load competing for the
same device. The `storage_queue_channel_overflow` metric is what would show it, which is why
exposing it is an obligation rather than a suggestion. The 10 Gbit measurement was taken on a WSL2
virtual disk over 512 MiB and should be repeated on the NUC.

Independently: index cost at 381-463 bytes per entry is three times the incumbent's. If a
deployment target appears where that is prohibitive, the trade is a larger slice size (ADR 0004)
before it is a different store.
