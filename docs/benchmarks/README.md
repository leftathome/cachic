# Benchmarks

## The protocol (plan section 9)

Parity with `lancachenet/monolithic` is a claim about two engines on the same hardware against the
same data volume, in **alternating runs**. Alternating matters: disk layout, page cache state and
thermal conditions all drift, and running one engine to completion and then the other attributes
that drift to the engine.

| Scenario | Measures |
|---|---|
| S1 | Warm, single client, whole object — Gbps, CPU, RSS |
| S2 | Warm, 32 clients, same object — aggregate Gbps, p50/p99 TTFB |
| S3 | Warm, 32 clients, 32 distinct objects — plus disk IOPS |
| S4 | Cold fill, 8 clients, same object — **upstream amplification** |
| S5 | Random 64 KiB–8 MiB ranges into large objects — the Windows Update shape |
| S6 | Restart with a populated cache — time to first hit |
| S7 | Eviction at cap, 24 h mixed replay — hit ratio, latency stability |

Reference environment: one amd64 NUC with NVMe as the cache host, a second host as the 10 GbE
client, and the origin behind `tc netem` at 1 Gbps / 20 ms to emulate a WAN link.

## Running the harness

```sh
cargo run --release --example bench -- --scenario all --clients 32 --object-mib 20480
```

It emits CSV on stdout, with the CPU model recorded in the first rows. Every number is only valid
for the hardware it was taken on, which is why the harness records that hardware rather than
leaving it to whoever commits the file.

The harness drives cachic only. It deliberately does **not** drive monolithic: orchestrating two
engines against one volume in alternating runs belongs to the benchmark host, not to a binary that
would have to shell out to Docker to do it. `docs/benchmarks/dev/` holds development-host runs;
the parity report will live alongside it once it exists.

## What has been measured

**Nothing against monolithic yet.** The parity report is TASK-25's remaining work and needs the
reference hardware. Until it exists, the floor constant in the performance gate is a provisional
backstop rather than nginx's measured throughput — see [ADR 0009](../adr/0009-performance-floor.md).

Two development-host runs exist and should be read as shape, not as results:

- [`m0/`](m0/README.md) — the M0 spike measurements, including a benchmark that produced
  confident, reproducible, completely wrong numbers, and what was wrong with it.
- [`dev/results.csv`](dev/results.csv) — the current harness across S1–S6 on a Ryzen 5 7600X under
  WSL2, with origin, proxy and clients all on the same box.

The one number in that run that is hardware-independent, and the one worth caring about:

```
S4,upstream_amplification,1.00,ratio
```

Eight clients starting the same cold object caused exactly one object's worth of upstream traffic.
Perfect coalescing is what the design claims over nginx's `proxy_cache_lock`, and it is a ratio
rather than a rate, so a slow machine cannot flatter it.

## Reading a result honestly

- Publish losses as well as wins. A report that shows only favourable scenarios is not a parity
  claim.
- Record the hardware, the versions and the exact commands. A benchmark nobody can reproduce
  proves nothing.
- Watch for numbers that are too good. The M0 report has a worked example of a measurement that
  reported 20 Tbps because every cache entry shared one buffer.
