# M0 measurements

Produced by TASK-04 to answer the M0 questions and to give the ADRs something to cite.

## Status: provisional, and the go/no-go is NOT resolved

Two things must be read before any number here is used.

**1. This is not the reference hardware.** The M0 exit criterion is specified on an amd64 NUC
with NVMe, with clients on a separate 10 GbE host. Everything below was taken on a WSL2 virtual
machine with the mock origin, the proxy and the load generator all on the same box, contending for
the same cores and the same virtual disk. Treat these as a shape, not as a result. The NUC and
Synology runs are still outstanding.

**2. The store does not survive restart, and it is foyer's defect rather than our misuse.** See
"Blocking finding" below, which includes a reproducer written directly against foyer's public API.
The throughput numbers are worth having; the recovery numbers block the Rust + foyer bet.

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
cargo build --release --bin measure --features measure

measure --dir /var/tmp/cachic codec        --slice-mib 1 --iterations 512
measure --dir /var/tmp/cachic store        --slice-mib 1 --entries 512 --mem-mib 2048 --disk-mib 4096 --block-mib 64
measure --dir /var/tmp/cachic store        --slice-mib 1 --entries 512 --mem-mib 64   --disk-mib 4096 --block-mib 64
measure --dir /var/tmp/cachic --direct store --slice-mib 1 --entries 512 --mem-mib 64 --disk-mib 4096 --block-mib 64
measure --dir /var/tmp/cachic index-memory --entries 100000,1000000
measure --dir /var/tmp/cachic recovery     --slice-mib 1 --entries 512
measure --dir /var/tmp/cachic proxy        --clients 8 --object-mib 256 --slice-mib 1 --mem-mib 1024 --rounds 3
measure --dir /var/tmp/cachic proxy        --clients 8 --object-mib 256 --slice-mib 1 --mem-mib 32   --rounds 2
```

Raw output is in `results.csv`.

## Blocking finding: the disk tier does not survive close and reopen

Writing 512 slices of 1 MiB, closing the store cleanly, reopening it, and reading the same keys
back recovers **30 of 512 slices (5.9%)**. Reopening takes 1 ms, which is itself the tell: nothing
is being scanned or rebuilt.

The same effect appears in every store run: 68 of 128 slices missing at one size, 799 and 1088 of
2048 at another. It is present with direct IO and with buffered IO, at 16 MiB and 64 MiB blocks,
and with a memory tier both larger and smaller than the data.

### This is foyer, not our usage

The obvious suspicion was that we hold foyer wrong - in particular that populating the cache
through `get_or_fetch` (which is how a proxy fills a cache as a side effect of serving) is not a
supported way to write. `cargo run --release --bin foyerprobe` tests that directly. It uses
foyer's public API with no cachic types involved: plain `u64` keys and `Vec<u8>` values, no slice
codec, no store wrapper.

| Populated with | Policy | Recovered |
|---|---|---|
| `insert` | `WriteOnInsertion` | 120 / 256 (46.9%) |
| `get_or_fetch` | `WriteOnInsertion` | 75 / 256 (29.3%) |
| `insert` | `WriteOnEviction` | 71 / 256 (27.7%) |
| `get_or_fetch` | `WriteOnEviction` | 89 / 256 (34.8%) |
| `insert`, 4 KiB entries | `WriteOnInsertion` | 2048 / 4096 (50.0%) |

Every trial writes far less data than the disk tier holds (256 MiB into 1 GiB; 16 MiB into
256 MiB), so nothing is being evicted for capacity. Every trial sets `flush_on_close(true)`,
uses buffered IO, and closes cleanly before reopening.

So it is not our slice codec, not our store wrapper, not `get_or_fetch`, not the cache policy, not
direct IO, not `flush_on_close`, not the IO throttle (unlimited by default), and not disk capacity.
`RecoverMode` already defaults to `Quiet`, which recovers while silently skipping errors - which
would hide precisely this.

The exact 50.0% in the small-entry trial is the most suggestive number here. A clean one-in-two
ratio looks like a systematic indexing or scanning defect rather than a tuning problem.

**Conclusion: this is foyer 0.22.4 behaviour, reproducible through its documented public API.**
The next action is an upstream issue carrying `foyerprobe` as the reproducer, not further local
guessing. It remains possible that some required configuration is not discoverable from the
builder API or the docs; if so, that is still a finding, and the answer will come from upstream
faster than from us.

This matters because FR-43 is a P0: "serve hits within seconds; full index rebuilt in the
background; no dependency on a clean shutdown". What was measured is worse than a dependency on a
clean shutdown - the shutdown *was* clean. A cache that keeps under half its contents across a
restart is not a capacity tier, and on the 2 TB caches this project targets, refilling that from
the internet is the entire problem the product exists to avoid.

### Second defect: close() hangs after failed fetches

`HybridCache::close()` does not return within 20 seconds after a read pass in which some fetch
closures returned errors. It returns promptly when every fetch succeeds. That threatens FR-62
(graceful shutdown within a bounded time) and is why the harness now wraps `close()` in a timeout.

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

- File the recovery defect upstream with `foyerprobe` as the reproducer, and decide between
  waiting for a fix, carrying a patch, or moving to the fallback store design (ADR 0003).
- File the `close()` hang after failed fetches.
- Re-run everything on the amd64 NUC with NVMe, clients on a separate 10 GbE host.
- Re-run the direct versus buffered IO comparison on real NVMe; the WSL2 answer probably inverts.
- Synology NFS over 10 GbE, as a second storage shape.
- Allocator comparison (mimalloc versus system) - not yet run.
- Recovery time for a 500 GB cache, once recovery works at all.
