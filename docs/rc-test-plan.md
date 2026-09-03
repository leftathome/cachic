# Release-candidate test plan

Everything cachic claims that cannot be proven on a developer machine. Each item states what is
being validated, how to run it, and — most importantly — **what result would falsify the claim**,
so a run produces a verdict rather than a number nobody can interpret.

Deliberately environment-agnostic: no addresses, hostnames, cluster names or credentials appear
here. Substitute your own and keep them out of this repository.

## Before starting

Record, and keep with the results:

- cachic version (the tag), image digest, and chart version
- CPU, RAM, and the storage device backing the cache volume
- Network between client and cache, and between cache and origin
- `lancachenet/monolithic` version, for the comparison runs

Every throughput number is only valid for the hardware it was taken on. A result filed without its
hardware is not a result.

---

## A. Parity against monolithic

**Claim.** As good and fast as nginx (the project's floor standard, ADR 0009).

**This is the most important item in the plan**, because the performance gate currently enforces a
provisional constant rather than a measured one. Until this runs, that gate catches catastrophic
regressions but does not enforce the standard it is named for.

### A1. Setup

Both engines against the **same data volume** on the **same host**, in **alternating runs**.
Alternating matters: disk layout, page-cache state and thermal conditions drift, and running one
engine to completion and then the other attributes that drift to the engine.

Clients on a separate host. The origin behind a bandwidth and latency shaper to emulate a WAN
link — the reference protocol is 1 Gbps with 20 ms of added latency.

Wipe the cache volume between engines. Use an identical `cache-domains` list for both.

### A2. Scenarios

Run each against cachic, then monolithic, then cachic again, and take the best of each engine's
runs. Throughput noise is one-sided: interference only ever makes you slower.

| | Scenario | Primary measure |
|---|---|---|
| S1 | Warm, single client, one large object | Gbps, CPU %, RSS |
| S2 | Warm, 32 clients, same object | Aggregate Gbps, p50/p99 TTFB |
| S3 | Warm, 32 clients, 32 distinct objects | Same, plus disk IOPS |
| S4 | Cold fill, 8 clients, same object | **Upstream amplification** |
| S5 | Random 64 KiB–8 MiB ranges into large objects | Hit ratio, amplification, p99 |
| S6 | Restart with a full cache | Time to first hit, time to full index |
| S7 | Eviction at cap, 24 h mixed replay | Hit-ratio and latency stability |

```sh
cachic-bench --scenario all --clients 32 --object-mib 20480 --dir /path/to/scratch
```

The harness ships as the `bench` example and emits CSV with the CPU model in its first rows.

### A3. What falsifies the claim

- **Any scenario where cachic is more than 5% slower than monolithic.** That is a floor breach and
  blocks the release, not a note for later.
- **S4 upstream amplification above 1.05.** Coalescing is the headline claim over nginx's
  `proxy_cache_lock`. It measures 1.00 in development; a real number materially above that means
  it is not working under real conditions.
- **S6 time-to-first-hit above a few seconds**, or a full index rebuild taking longer than five
  minutes per 2 TB (NFR-6).
- **S7 hit ratio degrading over the replay**, which would indicate an eviction pathology.

### A4. What to send back

The CSV from both engines, the hardware table, and monolithic's **S2 aggregate Gbps** figure
specifically — that number becomes `DEFAULT_FLOOR_GBPS` in the performance gate, replacing the
provisional constant.

---

## B. Kubernetes deployment

**Claim.** Installs on a Talos/Flux cluster from ten values or fewer (G6, FR-71).

### B1. Install

Deploy from the release artifact, not from a local build. Apply the Flux example with your own
load-balancer address and storage settings substituted.

### B2. Checks

- The pod reaches Ready. Note how long, particularly on a cache volume with existing data.
- `helm test` passes — it probes the heartbeat and readiness.
- The `X-LanCache-Processed-By` header appears on responses.
- Metrics are scraped and the Grafana dashboard populates every panel with real data.
- Client source addresses appear in the access log, not node addresses. If they do not,
  `externalTrafficPolicy` is not `Local`.

### B3. Restart and upgrade

- Delete the pod. It must come back serving from the existing volume without a refill.
- Change an unrelated value and let Flux roll it. The cache must survive.
- **Deliberately** change `cache.sliceSize` and confirm the pod refuses to start with a message
  naming both values and `FORCE_CONFIG`. A pod that starts here is a bug: it would reinterpret
  existing slices under a new size.

### B4. What falsifies the claim

- Needing more than ten values for a straightforward install.
- The cache not surviving a pod replacement.
- A slice-size change being accepted silently.

---

## C. Real client and ecosystem compatibility

**Claim.** Functional parity for every service in `cache-domains`; prefill tools and dashboards
work unchanged (G1, G4).

### C1. Prefill tools

Run SteamPrefill, and the Epic and Battle.net prefill tools, through the cache.

- Each must **detect** the cache — that is the heartbeat endpoint working.
- Each must complete without errors.
- A second run of the same content must be served from cache: watch upstream bytes, which should
  fall close to zero.

### C2. Real clients

Download the same content on two machines. The second should be served at LAN speed.

Cover at least: a Steam title, a Windows Update cycle, a Blizzard/Battle.net title, and a console
(PlayStation or Xbox). The console is worth including specifically — consoles with hard-coded DNS
bypass the cache silently, and it is the most common deployment complaint in the ecosystem.

### C3. LANCache Manager

Set `LOG_FORMAT=lancache` and point LANCache Manager at the log.

- Its log-derived views must populate.
- Its cache-directory features will **not** work, and this is expected: the store is a different
  shape. Confirm it degrades rather than crashing.

### C4. What falsifies the claim

- A prefill tool not detecting the cache — the heartbeat contract is broken.
- Any service caching on monolithic but not on cachic. Capture the request and the response
  headers; it is almost certainly a cache-key or service-matching difference.
- **Any corrupted download.** A client that installs and then fails a hash check is the most
  serious possible finding and should stop the release immediately.

---

## D. Soak

**Claim.** Zero corrupt bytes served over seven days (NFR-7).

### D1. Run

Seven days of continuous mixed traffic, with a working set larger than the disk tier so eviction
runs throughout. A soak that never evicts does not test eviction.

```sh
cachic-soak --seconds 604800 --clients 32 --disk-mib <size> --dir /path/to/scratch
```

Every read is verified against the generator, so corruption fails the run at the moment it appears
rather than being noticed later in aggregate.

Alongside, ideally, real client traffic through the same instance.

### D2. Watch throughout

| | Expectation |
|---|---|
| `cachic_checksum_failures_total` | Exactly zero |
| `foyer_storage_inner_op_total{op="channel_overflow"}` | Zero, or explained by a known fill burst |
| Process RSS | Flat. Steady growth over days is a leak |
| Hit ratio | Stable once the cache is full |
| Disk headroom | Stable; the guard should hold the cap |

### D3. What falsifies the claim

- **One integrity failure.** NFR-7 is absolute; the harness exits non-zero on the first.
- RSS growing steadily across the window.
- A non-zero overflow counter under ordinary fill rates, which means the cache is silently
  declining to store content.

---

## E. Fill rate

**Claim.** The upstream fill bar is 200 Mbit/s, with tuning available well beyond it.

Development measurements show writes being silently dropped above roughly 600 MiB/s at foyer's
default flusher settings; cachic ships four flushers and a 128 MiB buffer pool, which covered
every rate tested up to 10 Gbit.

### E1. Run

Fill at the bar and at whatever your link actually provides. Then, if the link allows, at 1, 2.5
and 5 Gbit.

### E2. What falsifies the claim

`foyer_storage_inner_op_total{op="channel_overflow"}` **non-zero at or below 200 Mbit/s**. That is
the shipped configuration failing at the stated bar and blocks the release.

Above the bar, a non-zero counter is a tuning finding: report the rate at which it starts, and
raise the flusher count and buffer pool.

---

## Reporting

For each section: **pass**, **fail**, or **not run**, with the numbers and the hardware. Publish
losses as well as wins — a report showing only favourable scenarios is not a parity claim.

The three results that change the code:

1. monolithic's S2 aggregate Gbps → the performance gate's floor constant.
2. The rate at which the overflow counter becomes non-zero → the shipped flusher defaults.
3. Any service caching on monolithic but not on cachic → a per-service rule.
