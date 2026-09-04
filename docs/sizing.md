# Sizing

Three starting points, and the two numbers you need to move away from them.

Everything here was measured on one machine, in a container, against a mock origin: 12 vCPU,
15 GiB RAM, a virtualised disk, clients on the same host. **The absolute throughput is that
machine's, not yours.** The two ratios travel better than the absolutes, and they are what the
table is built from:

| Measured | Value |
|---|---|
| Resident memory | `CACHE_MEM_SIZE` **+ ~700 MiB**, flat |
| CPU | **~0.5 cores per Gbps** served |

The memory overhead was 693 MiB at a 512 MiB tier, 704 MiB at 2 GiB and 648 MiB at 4 GiB, and did
not move between 8, 32 and 64 concurrent clients. It is a constant, not a proportion — so the RAM
tier is the only term you control, and doubling it costs exactly its own size.

## T-shirt sizes

| | Clients | `cache.memSize` | `cache.diskSize` | `requests.memory` | `limits.memory` | `requests.cpu` | `limits.cpu` |
|---|---|---|---|---|---|---|---|
| **Small** — a household, a few machines patching | up to 8 | `512m` | whatever you want cached | `1500Mi` | `2Gi` | `500m` | `2` |
| **Medium** — a LAN party, a small office | up to 32 | `2g` | " | `3Gi` | `4Gi` | `1` | `4` |
| **Large** — an event, a lab, a campus segment | 64+ | `4g` | " | `5Gi` | `6Gi` | `2` | `6` |

Medium is the chart default.

Measured peaks behind those numbers: 1205 MiB at Small, 2752 MiB at Medium, 4744 MiB at Large.
Requests sit just above the measured peak; limits leave roughly a third of headroom for a fill
burst. Throughput was 3.5-4.0 Gbps in all three, so on this hardware the tier did not decide
speed — see below.

`diskSize` is not in the sizing calculation. It is how much content you want to keep, capped by
the volume, and it costs no memory: the index is a redb file on disk, not a resident map.

## Choosing the RAM tier

**Bigger is not faster.** Holding the working set constant and varying only the tier:

| `CACHE_MEM_SIZE` | Throughput |
|---|---|
| 512m | 5.09 Gbps |
| 2g | 4.66 Gbps |
| 4g | 5.01 Gbps |

That is one noise band. The RAM tier is a hot-slice accelerator, and past a modest size the kernel
page cache is already doing the work — with the difference that page cache is *reclaimable* and
the RAM tier is not. Memory you hand to `memSize` is memory the host cannot take back under
pressure, so oversizing it trades a reclaimable cache for an unreclaimable one.

Start at the table. Raise `memSize` only if `foyer_memory_*` shows the tier evicting hot slices
and you have RAM the host is not otherwise using.

## Fill rate, not client count

Client count barely moves memory; **fill rate** is what strains the disk tier. `CACHE_FLUSHERS`
(4) and `CACHE_BUFFER_POOL` (128 MiB) set the write rate the cache absorbs before foyer starts
dropping writes rather than queueing them. The shipped values covered every rate tested to
10 Gbit.

Watch `foyer_storage_inner_op_total{op="channel_overflow"}`. Non-zero means slices are being
silently discarded: raise both together.

## Kubernetes notes

- `fsGroup` must be set or a first install onto dynamically provisioned block storage cannot write
  its state file. The chart sets it; do not remove it.
- Set `limits.memory` from the table, not from `memSize` alone. The overhead is real and constant,
  and a limit equal to the tier will be OOMKilled.
- The metric label is `cdn_service`, not `service` — see [known limitations](known-limitations.md)
  and the dashboard.

## Re-measuring on your own hardware

The harnesses ship in the `cachic-tools` tarball attached to each release:

```sh
# a mock origin plus a DNS server that points every CDN name at it
origin --address <addr> --http-port 80 --dns-port 53

# drive a running cache and report throughput and TTFB percentiles
loadtest --target http://<cache>:80 --clients 32 --seconds 300 \
         --objects 24 --object-mib 256
```

Sample RSS from outside the process while that runs; the peak under load is the number that
matters, not the value at idle.
