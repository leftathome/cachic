# orac — the first parity run against monolithic

Taken 2026-09-02/03 on the `orac` homelab cluster while executing
[`docs/rc-test-plan.md`](../../rc-test-plan.md) against v0.1.0-rc1. Findings from
the same run are in [`docs/rc-test-results.md`](../../rc-test-results.md).

This is the run [`../README.md`](../README.md) says will exist one day:

> **Nothing against monolithic yet.** The parity report is TASK-25's remaining
> work and needs the reference hardware.

It is **not** the reference hardware, and it does not settle the performance
gate. Read the limits before quoting anything from `results.csv`.

## What was run

Both engines on the **same host**, same load-balancer address, same origin, same
driver, cache volume **wiped between engines**, alternating. S1–S5 on both, S6 on
cachic. S7 was not run.

| | |
|---|---|
| Cache host | johnny — Talos, amd64, 40g disk tier, 1m slices |
| cachic | `ghcr.io/leftathome/cachic:0.1.0-rc1`, chart `0.1.0-rc1` |
| monolithic | `lancachenet/monolithic@sha256:37f28b36…` |
| Client | in-cluster pod on a different node, 16 clients, 1 GiB objects |
| Origin | in-cluster nginx, **no added latency** |

`lancachenet/monolithic` publishes **amd64 only** — verified by digest equality
across `crane digest --platform linux/amd64` and `linux/arm64`, so the flag is a
no-op and no arm64 variant exists. The comparison cannot be run at all on an
arm64-only deployment, which is worth a line in the test plan's prerequisites
since cachic itself ships arm64.

## Three limits, stated before the numbers

**1. Throughput here is link-bound, not engine-bound.** The client path tops out
near **0.89 Gbps** and every warm scenario sits just under it. These numbers show
neither engine is slower than the link. They cannot show which is faster, and
they cannot settle the §A3 five-percent bar. That needs the 10 GbE client the
protocol specifies.

**2. The origin had no added latency**, which matters more than it looks — see
the coalescing section below.

**3. 16 clients, not 32.** A 40g disk tier cannot hold 32 distinct 1 GiB objects
for S3 plus the other scenarios without evicting mid-run, which would have
measured eviction rather than S3.

## What the numbers say

**They tie on throughput.** Every scenario, both engines, within noise of each
other and of the link ceiling.

**cachic wins cold-fill latency by 15x, and this one is real.** Eight clients
start the same uncached 1 GiB object:

| | cachic | monolithic |
|---|---|---|
| S4 TTFB p50 | **135.8 ms** | 2039.1 ms |
| S4 TTFB p99 | **279.1 ms** | 2540.1 ms |

It is a latency ratio rather than a rate, so a faster link would not flatter it.
This is the design claim showing up: cachic streams to every waiter from the
first slice, where `proxy_cache_lock` makes the other seven wait on the first
request. For a room full of clients starting the same download, that is the
difference a user feels.

**Both engines were byte-perfect.** Zero integrity failures in 41 checked
transfers each, every body verified against the origin's digest.

**Index recovery is fast.** 397.5 ms to recover 639 blocks / 21569 entries on a
populated cache, against an §A3 budget of 5 minutes per 2TB. Cache survived pod
replacement with **zero refill** — the origin's request count for a cached object
was 1024 before the delete and 1024 after.

**Memory is the gap.** monolithic held **397 MiB** peak through the identical
workload that took cachic to **5.0–5.7 GiB**, and cachic was OOMKilled outright
at the chart's shipped 4Gi default. Some of that is architectural — nginx leans
on the kernel page cache, cachic carries its RAM tier in-process. Reaching 2.5x
the configured 2g tier, and dying at the chart's own default under 16 clients, is
not. That is `F11` in the findings.

## The claim that did not reproduce

Upstream amplification came out at **exactly 1.000 for both engines** — 8
clients, 1024 origin slice requests, 1 073 741 824 bytes, identical on each.
[`../README.md`](../README.md) presents perfect coalescing as "what the design
claims over nginx's `proxy_cache_lock`". On this rig nginx coalesces perfectly
too.

**This is a caveat, not a refutation.** Our origin is in-cluster with no added
latency, which is the condition most favourable to `proxy_cache_lock`: the lock
holds and waiters never time out. The protocol specifies the origin behind
`tc netem` at 1 Gbps / 20 ms precisely because that is where nginx's 5-second
lock timeout expires and spills duplicate upstream fetches.

**A shaped origin is the test that separates the two engines here, and we did not
run it.** Anyone repeating this should add the shaper before concluding anything
about coalescing parity — including before concluding in cachic's favour.

The S4 TTFB gap is the honest version of the same story: nginx does eventually
serve all eight clients from one upstream fetch, but it makes seven of them wait
two seconds to start.

## Reproducing

The driver and rig are not in this repo — cachic's own harness
(`examples/bench.rs`) states in its header that it drives cachic and
deliberately does not drive monolithic, so a comparison needs something else.
The one used here treats both engines as what they are on the wire, an HTTP
caching proxy keyed on `Host`:

- a mock origin serving **one** generated 1 GiB file for **every** path, so N
  distinct URLs are N distinct cache keys backed by one file — the origin stays
  small while the cache under test stores tens of gigabytes — logging
  `$body_bytes_sent` so amplification is measured rather than inferred;
- a CoreDNS beside it rewriting `lancache.steamcontent.com` (the sole entry in
  `uklans/cache-domains` `steam.txt`) to that origin, so each engine's own
  built-in Steam rules classify the traffic and neither needs a custom domain
  list;
- a stdlib-only Python driver emitting this CSV.

It lives in the operator's gitops repo at `docs/recipes/cachic-rc1-bench/`
(`rig.yaml`, `bench.py`, `monolithic.yaml`). It is worth upstreaming in some
form: without it there is no way for anyone to produce the parity report this
directory was waiting for.

One wrinkle worth knowing if you rebuild it: steering cachic's upstream to the
mock origin required overriding the **pod's** resolver, because `UPSTREAM_DNS`
does not control where cachic connects. That is `F10`, and it is a release
blocker in its own right.
