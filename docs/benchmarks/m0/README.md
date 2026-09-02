# M0 measurements

Produced by TASK-04 to answer the M0 questions and to give the ADRs something to cite.

## Status: provisional hardware; the store go/no-go is resolved in foyer's favour

Two things must be read before any number here is used.

**1. This is not the reference hardware.** The M0 exit criterion is specified on an amd64 NUC
with NVMe, with clients on a separate 10 GbE host. Everything below was taken on a WSL2 virtual
machine with the mock origin, the proxy and the load generator all on the same box, contending for
the same cores and the same virtual disk. Treat these as a shape, not as a result. The NUC and
Synology runs are still outstanding.

**2. An earlier version of this report was wrong about the store.** It claimed foyer lost half its
contents across a restart. The cause was a benchmark writing at 2.4 GB/s with no backpressure; see
"Resolved finding" below. foyer is fit for purpose and the Rust + foyer bet holds.

## Environment

| | |
|---|---|
| CPU | AMD Ryzen 5 7600X, 6 cores / 12 threads |
| Memory | 15 GiB available to the VM |
| Kernel | 6.6.87.2-microsoft-standard-WSL2 |
| Storage | ext4 on a 1 TB WSL2 virtual disk (`/dev/sdd`), not bare NVMe |
| Toolchain | rustc 1.98.0, release profile with LTO and `codegen-units = 1` |
| foyer | 0.22.4 |
| Date | 2026-09-01 |

The cache data directory was always on native Linux storage. Running it on a `/mnt/c` DrvFs path
measures the Windows filesystem bridge and is meaningless.

## Reproducing

```sh
cargo build --release --example measure

cargo run --release --example measure -- --dir /var/tmp/cachic codec        --slice-mib 1 --iterations 512
cargo run --release --example measure -- --dir /var/tmp/cachic store        --slice-mib 1 --entries 512 --mem-mib 2048 --disk-mib 4096 --block-mib 64
cargo run --release --example measure -- --dir /var/tmp/cachic store        --slice-mib 1 --entries 512 --mem-mib 64   --disk-mib 4096 --block-mib 64
cargo run --release --example measure -- --dir /var/tmp/cachic --direct store --slice-mib 1 --entries 512 --mem-mib 64 --disk-mib 4096 --block-mib 64
cargo run --release --example measure -- --dir /var/tmp/cachic index-memory --entries 100000,1000000
cargo run --release --example measure -- --dir /var/tmp/cachic recovery     --slice-mib 1 --entries 512
cargo run --release --example measure -- --dir /var/tmp/cachic proxy        --clients 8 --object-mib 256 --slice-mib 1 --mem-mib 1024 --rounds 3
cargo run --release --example measure -- --dir /var/tmp/cachic proxy        --clients 8 --object-mib 256 --slice-mib 1 --mem-mib 32   --rounds 2
```

Raw output is in `results.csv`.

## Resolved finding: foyer drops disk writes when the writer outruns the flusher

An earlier version of this report claimed the store lost more than half its contents across a
clean restart, and called it a foyer defect. **That was wrong, and the error was in the
benchmark.** The record is kept here because the way it was wrong is instructive.

### What is actually happening

foyer silently discards a disk write when the submit queue is saturated, incrementing
`storage_queue_channel_overflow`, and returns nothing to the caller. For a cache that is a
reasonable design: a dropped write is a future miss, not data loss. The behaviour is entirely
governed by write rate:

| Insert rate | Entries kept |
|---|---|
| 2,442 MiB/s (no pacing) | 46.9% |
| 303 MiB/s | **100%** |
| 88 MiB/s | **100%** |
| 38 MiB/s | **100%** |

Same capacity, same block size, same memory tier, same policy. Pace the writer below the
flusher's drain rate and nothing is lost.

### Why it looked like a restart-durability problem

Two mistakes compounded.

First, the harness only counted entries *after* a close and reopen, which cannot distinguish
"dropped on the way to disk" from "written and then not recovered". Counting live, before the
close, settles it immediately: the live count and the post-reopen count match, so recovery was
never at fault. Recovery works correctly.

Second, a large memory tier masks the drops. With a 512 MiB memory tier the live count is 100%
while the post-reopen count is 81.2% - entries sat in RAM, so reads succeeded, while their disk
writes had been discarded. The very first run of this harness used a 4 GiB memory tier and
reported zero misses, which read like a clean control and was in fact the symptom being hidden.

The underlying benchmark error: inserting at memory speed with no backpressure, a rate no real
cachic workload produces. Filling from a CDN over a domestic connection is tens of MiB/s, roughly
two orders of magnitude below where dropping begins.

### Sustainable ingest rate, and where the tuning knobs are

**The success bar for upstream fill is 200 Mbit/s** (owner's call, 2026-09-01). That is about
24 MiB/s, which sits roughly five times below the lowest rate tested here and about twenty-five
times below the highest rate that retains everything on foyer's defaults. At the bar, this is a
non-issue with a wide margin.

It is measured and documented anyway because fibre at 1-10 Gbit/s is now ordinary, some operators
will run well above the bar, and the failure is silent. Everything below is tuning guidance for
those users, not a blocker for shipping.

Note this concerns the **ingest** path only. Serving cached content to LAN clients is a different
path with its own target (NFR-1, >= 1.1 GB/s) and is unaffected.

Converted to fill rates and measured against foyer's defaults (1 flusher, 16 MiB buffer pool),
512 MiB written into a 4 GiB disk tier with a deliberately small 64 MiB memory tier so RAM hits
cannot mask a dropped write:

| WAN | Fill rate | Entries kept, foyer defaults |
|---|---|---|
| 1 Gbit | 119 MiB/s | 100% |
| 2.5 Gbit | 298 MiB/s | 100% |
| 5 Gbit | 596 MiB/s | 100% |
| **10 Gbit** | **1192 MiB/s** | **90.0%** |

Defaults are clean through 5 Gbit. At 10 Gbit one slice in ten is silently discarded: the client
still gets its bytes at full speed, the cache simply does not keep them, and without the overflow
metric nobody finds out. That last property is why the metric matters more than the tuning does.

It is entirely a configuration problem. At the same 1192 MiB/s target:

| flushers | buffer pool | submit queue | Entries kept |
|---|---|---|---|
| 1 (default) | 16 MiB (default) | 16 MiB (default) | 90.0% |
| 2 | 64 MiB | 64 MiB | **100%** |
| 4 | 128 MiB | 128 MiB | **100%** |
| 8 | 256 MiB | 256 MiB | **100%** |

The disk absorbed 1.19 GiB/s with full retention once tuned, so the device was never the limit -
foyer's single default flusher was. Note also that raising `submit_queue_size_threshold` alone does
nothing; an earlier sweep of 16/64/256/1024 MiB moved retention not at all. The queue is not the
bottleneck, the drain is.

**Recommendation: ship 2 flushers and a 64 MiB buffer pool as the default**, and document the
table above for operators tuning further. That is not required to meet the 200 Mbit/s bar - foyer's
own defaults clear it with room to spare - but it costs almost nothing, covers every fibre tier up
to 10 Gbit without the operator knowing the knob exists, and means the first person to plug this
into a 10 GbE homelab does not quietly lose a tenth of their cache. The knobs belong in the
configuration surface (TASK-07) with worked examples in the docs (TASK-32).

Caveats: measured on a WSL2 virtual disk, not NVMe, and over 512 MiB rather than a sustained
multi-hour fill with concurrent read load. Both should be repeated on the NUC.

### What this means for cachic

foyer is fit for purpose. FR-43 is not in danger. But three things follow and are not optional:

1. **`storage_queue_channel_overflow` and `storage_block_engine_enqueue_skip` must be exposed**
   through `/metrics` (FR-50). A cache that silently declines to cache is the worst failure mode
   this product can have, and it is invisible without these counters. This is now a TASK-13
   requirement, not a nice-to-have.
2. **The store must be built with more than one flusher.** foyer's defaults drop 10% of a 10 Gbit
   fill. Minimum 2 flushers and a 64 MiB buffer pool (TASK-11), exposed as configuration
   (TASK-07), and tested at fibre and prefill rates (TASK-20). This applies to ordinary WAN fills
   on fibre, not only to prefill.
3. **Benchmarks must not write at rates the product cannot produce.** The measurement harness
   should pace writes to a configured rate by default.

### Still open: close() hangs after failed fetches

`HybridCache::close()` did not return within 20 seconds after a read pass containing failed fetch
closures, and returned promptly when every fetch succeeded. This was observed before the write-rate
cause was understood and has not been re-examined since; it may share a root cause. It threatens
FR-62 and needs re-testing rather than reporting as-is.

## Throughput

### Slice codec

| Metric | Value |
|---|---|
| Encode, including xxh3 | 4,456 MiB/s (37 Gbps) |
| Decode, including checksum verification and a copy | 12,253 MiB/s (103 Gbps) |

The codec is not a bottleneck at any throughput this project targets.

### Store

With a memory tier larger than the working set, warm reads are effectively free:

| Configuration | Warm read | Durable write |
|---|---|---|
| Memory tier holds everything | 25,627 MiB/s (215 Gbps) | 936 MiB/s |
| Disk tier, buffered IO | 968-1,324 MiB/s (8-11 Gbps) | 3,069-4,105 MiB/s |
| Disk tier, direct IO | 481 MiB/s | - |

**Buffered IO reads roughly twice as fast as direct IO here** (968 vs 481 MiB/s). On a WSL2
virtual disk that is unsurprising, and it is the opposite of what a bare NVMe device would likely
show, so this specific comparison must be repeated on the NUC before `direct_io` gets a default.

Disk-tier reads at ~1 GiB/s sit right on NFR-1's 1.1 GB/s line, on hardware that is not the target.

### Index memory

| Entries | RSS per entry |
|---|---|
| 100,000 | 463 bytes |
| 1,000,000 | 381 bytes |

This is the number that sizes `CACHE_MEM_SIZE`, and it is worse than the incumbent. lancache's
rule of thumb - 1 MB of shared memory per ~8 GB of 1 MiB slices - works out to roughly 128 bytes
per entry. foyer costs about three times that.

Consequences at 1 MiB slices: a 2 TB cache is ~2M slices and ~760 MB of index; a 10 TB cache is
~3.8 GB. That is affordable on the target hardware but it is not free, it must be documented in
the sizing guide, and it argues for a larger default slice size on large caches.

Insertion ran at ~16,000 entries/s regardless of count, so populating a 2M-entry index takes about
two minutes of pure insert time.

### End to end through the spike proxy

8 concurrent clients, 256 MiB object, 1 MiB slices, mock origin on loopback:

| Metric | Memory tier | Disk tier |
|---|---|---|
| Hit throughput | 2.74-2.81 Gbps | 2.80 Gbps |
| TTFB p50 | 1.13-1.87 ms | 2.61-3.14 ms |
| TTFB p99 | 1.72-11.66 ms | 6.31-6.73 ms |
| RSS | ~1.24 GB (1 GiB memory tier) | ~1.01 GB (32 MiB memory tier) |

**The M0 exit criterion of >= 8 Gbps with 8 clients is not met here: we measured 2.8 Gbps.**

The memory tier and the disk tier produce the *same* end-to-end throughput, while the store
benchmark shows them differing by a factor of twenty. The bottleneck is therefore not the store -
it is the HTTP path, the copies between store and socket, and the fact that origin, proxy and
clients share six cores. That is a useful result: it says store choice is not what limits us at
this scale, and it says the next throughput work belongs in the serving path, not in foyer.

Whether 8 Gbps is reachable is genuinely unknown until this runs on the NUC with clients on a
separate host.

Other observations:

- **Coalescing holds end to end.** A 256 MiB object at 1 MiB slices produced exactly 257 upstream
  requests: 256 slices plus one probe. No amplification.
- **Cold-fill overhead was 3.1% in one run and 19.1% in another.** NFR-3 wants fill throughput at
  >= 95% of direct. The spread is too wide to conclude anything with the origin on loopback; this
  needs the WAN-emulated origin from the benchmark protocol.
- **RSS tracked the configured memory tier** in both configurations, which is the NFR-5 behaviour
  we want, though NFR-5 needs testing at real concurrency rather than 8 clients.
- **RAM-tier p99 TTFB reached 11.66 ms**, over NFR-2's 5 ms. On a contended box that is weak
  evidence, but it should be watched.

## What is still outstanding

- Re-test the `close()` hang after failed fetches now that the write-rate cause is understood.
- Pace writes in the measurement harness by default, and re-run the store numbers with pacing.
- Re-run everything on the amd64 NUC with NVMe, clients on a separate 10 GbE host.
- Re-run the direct versus buffered IO comparison on real NVMe; the WSL2 answer probably inverts.
- Synology NFS over 10 GbE, as a second storage shape.
- Allocator comparison (mimalloc versus system) - not yet run.
- Recovery time for a 500 GB cache, once recovery works at all.
