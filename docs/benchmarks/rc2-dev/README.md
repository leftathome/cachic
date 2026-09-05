# rc2 development-host measurements

The measurements behind [sizing.md](../../sizing.md) and behind the retraction of a parity claim
made from the rc1 results. Taken while fixing the rc1 findings, so they describe rc2 code — which is also rc5's: v0.1.0-rc2
published a release with no assets attached and could not be re-tagged, so v0.1.0-rc3 carries the
same contents with the release plumbing fixed.

**Hardware, and why it limits what this proves.** One machine: WSL2, 12 vCPU, 15 GiB RAM, a
virtualised disk, glibc 2.35 host. cachic ran in a container; `lancachenet/monolithic` ran in a
container beside it; the mock origin ran in a separate network namespace so both engines could
bind `:80` behind an identical DNS answer. Clients were on the same box. **Every absolute
throughput number here is that machine's, not a claim about yours.** The ratios travel; the
absolutes do not.

Commands:

```sh
origin --address 10.200.0.2 --http-port 80 --dns-port 53
loadtest --target http://127.0.0.1:<port> --clients 32 --seconds 60 \
         --objects 24 --object-mib 256
```

## Parity against monolithic

Identical load, identical origin, one engine at a time. 32 clients.

| Working set | State | cachic | monolithic | cachic / monolithic |
|---|---|---|---|---|
| 1 GiB | warm | 4.87 Gbps | 4.72 Gbps | 103% |
| 3 GiB | cold | 5.12 Gbps | 4.44 Gbps | 115% |
| 3 GiB | warm | 5.21 Gbps | 4.56 Gbps | 114% |
| 6 GiB | cold | 4.32 Gbps | 4.32 Gbps | 100% |
| 6 GiB | warm | 4.56 Gbps | 5.19 Gbps | **88%** |

cachic is at parity or ahead until the working set is several times the RAM tier, where it gives
up about 12% on warm serving. foyer reports ~1.6x read amplification on the disk tier — 41.8 GiB
read from disk to serve 26.7 GiB to clients on a warm read-only pass — which is the obvious
suspect and is not yet proven to be the cause.

**A retraction.** An earlier reading of this comparison put cachic at 62% of nginx. That number
compared a *cold* cachic against a differently configured monolithic, on a host that then had
about 7 GiB free rather than 12. Matched properly it does not reproduce. Cold and warm are
different claims and a number that does not say which is not a result. The 88% above is the real
soft spot and is smaller and narrower than the 62% suggested.

## Memory

Peak RSS under load, sampled from outside the container, working set roughly 3x the tier:

| `CACHE_MEM_SIZE` | Clients | Peak RSS | Overhead above tier |
|---|---|---|---|
| 512m | 8 | 1205 MiB | 693 MiB |
| 2g | 32 | 2752 MiB | 704 MiB |
| 4g | 64 | 4744 MiB | 648 MiB |

The overhead is **constant**, not proportional, and does not move with client count. That is the
basis for the sizing rule `memSize + 700 MiB`, and it replaces the chart's previous estimate of
roughly 400 bytes per stored slice, which predicted ~2.1 GiB where 2.9 GiB was needed and got rc1
OOMKilled at its own default limits.

CPU measured 123.6 CPU-seconds over 60.2 s of wall time at 4.08 Gbps: **2.05 cores, or ~0.5 cores
per Gbps.**

## Allocator

The rc1 report read climbing memory as a leak. It is not a leak — RSS plateaus — it plateaus far
above the configured tier, because glibc's per-thread arenas fragment badly with 1 MiB slice
buffers. Same soak, same 256 MiB tier:

| Allocator | RSS | Throughput |
|---|---|---|
| glibc default | 1740 MiB | 49.0 req/s |
| `MALLOC_ARENA_MAX=4` | 1065 MiB | 47.9 req/s |
| `MALLOC_ARENA_MAX=2` | 922 MiB | 47.4 req/s |
| **jemalloc** | **787 MiB** | 47.2 req/s |

Throughput differences there are inside the noise band. Checked separately on an idle machine with
the performance gate, interleaved to cancel drift, jemalloc was slightly *faster*: 4.08 and 3.46
Gbps against 3.72 and 3.27. jemalloc is now the default on gnu targets.

## RAM tier size does not buy throughput

Working set held constant at 3 GiB, only the tier varied, on a host with 12 GiB free:

| `CACHE_MEM_SIZE` | Throughput |
|---|---|
| 512m | 5.09 Gbps |
| 2g | 4.66 Gbps |
| 4g | 5.01 Gbps |

One noise band. Past a modest size the kernel page cache is already doing the work, with the
difference that page cache is reclaimable and the RAM tier is not.

## A theory that did not survive

foyer was suspected of paying a disk write every time an entry was demoted from RAM to disk, even
though the entry on disk had not changed. The eviction hook does call `store.enqueue` with no
"already present" check, which is what the code reads like. Measured on a warm read-only pass:
**81847 enqueues, 0 bytes written.** The enqueue path deduplicates. The theory was wrong, and the
earlier observation that suggested it — 26539 writes during a "warm" pass — came from a pass that
was still filling.
