# Definition of done for v1.0

Appendix C of the implementation plan, assessed honestly. Items are marked **done**, **blocked on
hardware**, or **outstanding**.

## Requirements

| | Status |
|---|---|
| All PRD P0 requirements implemented with tests | **Mostly done** — see the gaps below |
| All PRD P1 requirements implemented with tests | **Mostly done** |

Implemented and tested: the HTTP surface and range handling (FR-01, FR-05, FR-06, FR-07, FR-11,
FR-15), service matching and key normalisation (FR-02, FR-21), the dedicated resolver and address
guard (FR-03, FR-64), slicing and the fetch pipeline (FR-10, FR-12, FR-16), the `no_ranges` path
(FR-13, FR-32), generation-based invalidation and `If-Range` (FR-14, FR-17), coalescing and
disconnect semantics (FR-30, FR-31), the hybrid store and its integrity properties (FR-40 through
FR-45), the free-space guard (FR-46), metrics, logs and probes (FR-50, FR-51, FR-52, FR-53), the
admin API (FR-54), configuration (FR-60, FR-61, FR-62, FR-63), SNI pass-through (FR-08), and
packaging (FR-70, FR-71, FR-72).

Known gaps:

- **FR-09** — done. Per-service ceilings come from `max_inflight` in the rules file; a service
  without one is bounded by the global limit.
- **FR-22** — done. `STALE_ON_ERROR` (default on) serves cached slices through a transient
  upstream failure. It deliberately does not serve them through a 404.
- **FR-73** — static musl binaries. Blocked: foyer 0.22 does not compile for musl, and the
  upstream fix is one line. Binaries are glibc. See
  [known limitations](./known-limitations.md#no-static-musl-binaries).
- **FR-47, FR-23, FR-55** — nginx cache import, revalidation and OpenTelemetry are 1.x by design.

## Verification

| | Status |
|---|---|
| Benchmark report showing parity on S1–S7 | **Blocked on hardware** |
| Chart installed via Flux on the Talos cluster | **Blocked on hardware** |
| Grafana dashboard live | **Blocked on hardware** — the dashboard exists and its queries are tested against exported metrics |
| SteamPrefill, Epic and Battle.net prefill runs complete | **Blocked** — needs real credentials and clients |
| LANCache Manager log features working | **Blocked** — the format is tested field-by-field against monolithic's; the tool itself has not been pointed at us |
| 7-day soak with zero integrity failures | **Blocked on hardware** — the harness exists and passes over shorter runs |
| Docs site complete | **Done** |
| CHANGELOG and signed artefacts for amd64 and arm64 | **Outstanding** — the pipeline exists and has never been triggered |

## What "blocked on hardware" means

Six items need the reference environment: an amd64 NUC with NVMe, a second host as a 10 GbE
client, a Talos cluster, and real game-client credentials. None is blocked on code.

The parity benchmark is the most important of them, and it is the one the performance gate is
currently guessing at: `DEFAULT_FLOOR_GBPS` is a provisional backstop rather than nginx's measured
throughput on the same hardware. Until that run happens, the gate catches catastrophic regressions
but does not enforce the floor standard it is named for. See
[ADR 0009](./adr/0009-performance-floor.md).

## What has been verified

- **357 tests**, including a differential pass over random objects and ranges, cold and warm.
- **Upstream amplification of exactly 1.00** on a cold object with eight concurrent clients. This
  is the coalescing claim over nginx's `proxy_cache_lock`, and being a ratio rather than a rate it
  cannot be flattered by fast hardware.
- **Fuzzing** on four parsers, every push and nightly, which found a real slice-index overflow.
- **Crash recovery** against the real binary under SIGKILL, asserting no corruption.
- **A soak** at three times the disk tier, so eviction runs continuously: 28 GB served, zero
  integrity failures, flat RSS.
- **The container** built and run, 13.2 MB compressed, non-root on distroless.
- **The chart** rendered and validated against the Kubernetes schema in CI.

## Running the remaining verification

```sh
# On the NUC, with monolithic available for the alternating comparison.
cargo run --release --example bench -- --scenario all --clients 32 --object-mib 20480

# A long soak. Zero corrupt bytes or it fails.
cargo run --release --example soak -- --seconds 604800 --clients 32 --disk-mib 500000
```

Then replace `DEFAULT_FLOOR_GBPS` in `crates/cachic/tests/perf_gate.rs` with monolithic's measured
figure, and this document stops guessing.
