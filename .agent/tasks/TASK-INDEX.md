# Task Index

Derived from `docs/cachic-IMPLEMENTATION-PLAN.md` section 4. Status values: Not started /
In progress / Blocked / Done. Update the status column as work lands.

## M0 - Spike and ADRs (weeks 1-2)

| Task | Title | Status |
|---|---|---|
| [TASK-01](./TASK-01-repo-skeleton.md) | Repository skeleton and dev loop | Done |
| [TASK-02](./TASK-02-ci-baseline.md) | CI baseline | Done (image build deferred to TASK-15) |
| [TASK-03](./TASK-03-m0-spike.md) | M0 spike - hyper + reqwest + foyer sliced GET prototype | Done |
| [TASK-04](./TASK-04-m0-measurements.md) | M0 measurements | Done (provisional hardware; NUC/Synology runs outstanding) |
| [TASK-05](./TASK-05-pingora-vs-hyper.md) | Pingora vs hyper evaluation note | Done (ADR 0002) |
| [TASK-06](./TASK-06-m0-adrs.md) | M0 architecture decision records | Done (all eight accepted) |

**Exit criteria**: measured hit throughput >= 8 Gbps on the NUC with 8 clients; foyer index memory
per entry known; go/no-go on Rust confirmed; CI green on lint/test.

**Status: partially met.** CI is green, index memory is known (381-463 bytes/entry), and the
go/no-go on Rust + foyer is **confirmed** (ADR 0001, ADR 0003). Throughput was measured at 2.8 Gbps
on a WSL2 box with origin, proxy and clients colocated, not the NUC, so the >= 8 Gbps criterion is
untested rather than failed - it needs the reference hardware.

**Success bar for upstream fill: 200 Mbit/s** (owner, 2026-09-01), roughly 24 MiB/s. Higher rates
are a tuning story, not a gate. This bounds the ingest path only; serving hits to LAN clients keeps
its own target (NFR-1).

M0 initially reported a blocking store defect. That was a benchmark error: foyer drops disk writes
when the writer outruns the flusher, and the harness wrote at 2.4 GB/s with no backpressure, a rate
cachic never produces. See `docs/benchmarks/m0/README.md`. It leaves three obligations:
TASK-13 must expose `storage_queue_channel_overflow`, TASK-20 must test fills at the success bar,
and the measurement harness should pace writes. TASK-11 defaults above foyer's write-path settings
so fibre users are covered without tuning.

## M1 - MVP proxy, v0.1 (weeks 3-6)

| Task | Title | Status |
|---|---|---|
| [TASK-07](./TASK-07-config.md) | Configuration surface | Done |
| [TASK-08](./TASK-08-services.md) | Services - cache-domains, matcher, key normalisation | Done |
| [TASK-09](./TASK-09-proxy.md) | Proxy - server, router, headers, range parsing | Done |
| [TASK-10](./TASK-10-upstream.md) | Upstream client, resolver and guards | Done |
| [TASK-11](./TASK-11-store.md) | Store - foyer wrapper, slice codec, object index | Done |
| [TASK-12](./TASK-12-orchestrator.md) | Orchestrator - probe, slice plan, pipeline | Done |
| [TASK-13](./TASK-13-telemetry.md) | Telemetry - logs, metrics, health | Done |
| [TASK-14](./TASK-14-testkit.md) | Testkit - mockcdn, differ, load generator | Done |
| [TASK-15](./TASK-15-packaging-v0.1.md) | Packaging v0.1 - image and compose | Done (glibc not musl; see Dockerfile) |

**Exit criteria**: SteamPrefill and Epic prefill complete through the proxy; differential tests
pass; image runs as non-root on amd64 and arm64.

**Status: substantially met.** Differential tests pass over random objects and ranges, cold and
warm. The image runs as non-root on amd64 and is 13.2 MB compressed, well inside NFR-8's 40 MB.
Two gaps: arm64 has not been built here (TASK-26 sets up the multi-arch pipeline), and the prefill
runs need real credentials and a client, so they belong with TASK-33. Packaging deviates from the
plan by building against glibc rather than static musl - foyer 0.22.4 does not compile for musl;
the reason and the one-line upstream fix are documented in the Dockerfile.

## M2 - Robustness, v0.2 (weeks 7-9)

| Task | Title | Status |
|---|---|---|
| [TASK-16](./TASK-16-no-ranges-filler.md) | no_ranges path and object-level filler | Done |
| [TASK-17](./TASK-17-generations-stale-on-error.md) | Generations, stale-on-error, If-Range | Done |
| [TASK-18](./TASK-18-lifecycle-limits.md) | Disconnect semantics, graceful shutdown, limits | Done |
| [TASK-19](./TASK-19-admin-api.md) | Admin API and disk guard | Done (reload lands with TASK-29) |
| [TASK-20](./TASK-20-chaos-suite.md) | Chaos suite | Done (cgroup IO throttling deferred to the compose profile) |
| [TASK-21](./TASK-21-fuzzing.md) | Fuzzing in CI | Done |

**Exit criteria**: chaos suite green; 48-hour soak on the homelab with real clients.

**Status: met apart from the soak.** The chaos suite is green, fuzzing runs on every push and
nightly, and crash recovery is exercised against the real binary. The 48-hour soak needs the
homelab and real clients, so it belongs with TASK-33.

## M3 - Deployment and parity, v0.3 (weeks 10-12)

| Task | Title | Status |
|---|---|---|
| [TASK-22](./TASK-22-helm-chart.md) | Helm chart | Done (kind install is CI's job) |
| [TASK-23](./TASK-23-flux-k8s-docs.md) | Flux example and Kubernetes documentation | Done (Talos install needs the cluster) |
| [TASK-24](./TASK-24-lancache-log-compat.md) | lancache access-log format and ecosystem compatibility | Done (LANCache Manager smoke test needs the tool) |
| [TASK-25](./TASK-25-benchmark-harness.md) | Benchmark harness and parity report | Harness done; parity report needs the NUC |
| [TASK-26](./TASK-26-release-pipeline.md) | Release pipeline | Done (untriggered; no tag pushed) |

**Exit criteria**: chart installed on the Talos cluster via Flux with <= 10 values; benchmark
report shows parity on every scenario.

**Status: partially met.** The chart installs from seven values and its rendered manifests
validate against the Kubernetes schema in CI, but it has not been applied to the Talos cluster.
The benchmark harness runs all of S1-S6 and shows upstream amplification of exactly 1.00, but the
parity comparison against monolithic needs the reference hardware. Both gaps are hardware, not
code.

## M4 - v1.0 (weeks 13-16)

| Task | Title | Status |
|---|---|---|
| [TASK-27](./TASK-27-sni-passthrough.md) | SNI pass-through on 443 | Done |
| [TASK-28](./TASK-28-readahead-tuning.md) | Read-ahead tuning | Done |
| [TASK-29](./TASK-29-domain-auto-refresh.md) | cache-domains auto-refresh with hot reload | Done |
| [TASK-30](./TASK-30-grafana-dashboard.md) | Grafana dashboard | Done |
| [TASK-31](./TASK-31-service-rule-parity.md) | Per-service rule parity review | Done (real-traffic replay outstanding) |
| [TASK-32](./TASK-32-docs-site.md) | Documentation site | Done |
| [TASK-33](./TASK-33-soak-and-v1.md) | Soak, definition of done, v1.0 release | 0.1.0-rc1 tagged; validation runs on real hardware per docs/rc-test-plan.md |

**Exit criteria**: Appendix C definition of done.

**Status: assessed in `docs/definition-of-done.md`.** Six items are blocked on the reference
hardware and real client credentials; none is blocked on code. No tag has been pushed.

## Dependency Notes

- TASK-01 blocks everything. TASK-03 blocks TASK-04 and TASK-05; both block TASK-06.
- M1's TASK-12 is the integration point: it needs TASK-08, 09, 10 and 11 in place.
- TASK-14 (testkit) is promoted out of the TASK-03 spike rather than written from scratch.
- TASK-25 (benchmarks) and TASK-33 (soak) need hardware beyond a dev laptop.
