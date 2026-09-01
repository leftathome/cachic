# cachic — Implementation Plan

| | |
|---|---|
| **Status** | v0.1 |
| **Date** | 2026-09-01 |
| **Scope** | Delivers the PRD (`cachic-PRD.md`) through v1.0 |
| **Assumption** | One primary developer, part-time; contributors welcome after 0.2 |

## 0. Decisions

### 0.1 Greenfield, in Rust

**Decision:** build a new application in Rust, composed from `tokio` + `hyper` (HTTP), **foyer** (hybrid RAM/disk store) and a thin layer of our own orchestration. Revisit at the end of M0 if the spike falsifies the assumptions below.

Why Rust over Go for *this* project:

| Concern | Rust | Go |
|---|---|---|
| The hard component — a hybrid RAM+disk cache with region-based disk layout, eviction, request dedup, crash recovery, io_uring/psync IO, Prometheus metrics | **foyer** exists and is production-proven (RisingWave, Chroma, SlateDB, ZeroFS) | No equivalent; a custom store is ~3–5 weeks of careful work plus ongoing tuning |
| HTTP server/client plumbing | `hyper` 1.x is mature; Pingora available as an alternative | `net/http` is excellent and includes `sendfile` on hits |
| Large-buffer workloads (GB/s of 1 MiB slices) | No GC; `Bytes` gives zero-copy fan-out | Works, but needs pooling and GC tuning to keep latency flat |
| Cross-compilation / multi-arch | `cross`/`cargo-zigbuild`, musl static | Trivial |
| Contributor pool in the lancache community | Smaller | Larger |
| Dev velocity | Slower | Faster |

The tie-breaker is reuse: the PRD's "reuse community code" goal is best met by not writing a cache engine. The Go path remains documented in Appendix A so switching after M0 costs a day, not a month.

### 0.2 Why not contribute to an existing project

| Candidate | Verdict |
|---|---|
| `lancachenet/monolithic` | nginx; the thing we are replacing. We *do* contribute back: `cache-domains` rules, docs, and LANCache Manager backend support. |
| Apache Traffic Server (`slice` + `cache_range_requests` plugins) | The mature non-nginx answer to slice caching, but C++, large, and not a small 12-factor app. Zero-code fallback if the greenfield stalls. |
| Pingora (`pingora-cache`) | Cache integration is documented as experimental with volatile APIs and only a test-grade `MemCache` storage. The request model is one upstream response per downstream request; slicing needs N. Useful as server plumbing; evaluated in M0. |
| pingap (Pingora-based reverse proxy) | General API-gateway with a cache plugin; host/path locations, not transparent any-host caching. Bolting slice semantics on would fight its model. |
| Caddy + Souin (Go) | Key-value cache middleware for whole bodies; chunked multi-GB objects would be a rewrite of its storage layer. |
| `kixelated/steamcache` (Go) | Two commits, 2016; a data point that the idea is sound, not a base. |

### 0.3 Pingora vs hyper (resolved in M0)

Default is `hyper`. Pingora wins only if the M0 spike shows that `ProxyHttp::request_filter` short-circuiting plus Pingora's upstream connectors gives a cleaner slice orchestrator than hyper + `reqwest`, *and* its Linux-first stance is acceptable (macOS is best-effort, Windows preliminary — both fine for our tiers).

## 1. Architecture

### 1.1 Components

```
┌────────────────────────────────────────────────────────────────────────┐
│ cachic (one process)                                               │
│                                                                        │
│  listeners ─┬─ :80  HTTP/1.x server (hyper) ──▶ router ──▶ orchestrator │
│             ├─ :443 SNI pass-through (tokio TCP + ClientHello peek)    │
│             └─ :9090 admin (metrics, health, purge, reload)            │
│                                                                        │
│  services   : cache-domains loader, service rules, key normaliser      │
│  orchestrator: range parse → slice plan → fetch/serve pipeline         │
│  upstream   : HTTP client pool (reqwest/hyper), dedicated resolver,    │
│               per-host limits, retries, timeouts                       │
│  store      : foyer HybridCache<SliceKey, SliceValue> + object index   │
│  telemetry  : tracing (JSON) + metrics (Prometheus) + access log       │
└────────────────────────────────────────────────────────────────────────┘
```

### 1.2 Request flow

1. Parse request; take `Host` (port stripped). Match against services (compiled hostname matcher; exact + wildcard). No match → 404 or pass-through.
2. Non-`GET`/`HEAD` → pass-through. `HEAD` → answer from object index if known, else proxy.
3. Normalise the cache key per service rule → `object_id = blake3(identifier ‖ normalised_key)[..16]`.
4. Parse `Range` (single range only; multi-range → treat as full).
5. Look up object metadata (length, validators, generation, `no_ranges`). If unknown, issue a **probe**: fetch the first needed slice with `Range: bytes=a-b`; a `206` yields total length and validators from `Content-Range`/`ETag`/`Last-Modified`; a `200` marks the object `no_ranges` and switches to the full-stream path.
6. Compute slice indices `[i0..=i1]`. Send response headers (`200` or `206`, `Content-Length`, `Content-Range`, validators, `Accept-Ranges`, `X-Cache`, `X-LanCache-Processed-By`).
7. Pipeline: for `i` in `i0..=i1`, `store.fetch((object_id, gen, i), || upstream.fetch_slice(...))` with a bounded window (`READAHEAD_SLICES`); foyer's `fetch` coalesces concurrent misses per key. Write each slice's relevant byte range to the body in order.
8. On validator mismatch in any slice response: bump generation, abort the client stream (connection close → client retries), log and count.
9. Emit the access event with cache status (`HIT` all slices from store, `MISS` none, `PARTIAL` mixed).

The `no_ranges` path (FR-13/FR-32) uses an object-level filler registry (`DashMap<object_id, Arc<FillState>>`): one task streams the full body, cutting slices into the store and publishing per-slice readiness; other requests subscribe rather than fetch.

### 1.3 Storage model

- **Slice key**: `(object_id: [u8;16], generation: u32, index: u32)`.
- **Slice value**: small header (`magic`, `slice_size`, `total_len`, `etag`, `last_modified`, `content_type`, `generation`, `xxh3` of payload) + payload. Slices are self-describing (FR-44), so the object index is a rebuildable acceleration structure.
- **Store**: foyer `HybridCache` — memory tier sized by `CACHE_MEM_SIZE` (S3-FIFO or LRU), disk tier on `CACHE_DATA_DIR` with a file-based device sized by `CACHE_DISK_SIZE`. Eviction, admission, recovery, dedup and metrics come from foyer. Direct IO is a tunable; page-cache double-buffering is the thing to measure in M0.
- **Object index**: `redb` table `object_id → {key, len, validators, gen, no_ranges, created, last_seen}`; `last_seen` updated at most hourly per object; entries pruned by `CACHE_MAX_AGE`. Missing index entry ⇒ probe (cheap) and, if a slice is found in the store, the index is repaired from the slice header.
- **Config guard**: `slice_size` and store format version written to `CACHE_DATA_DIR/CONFIG`; mismatch aborts startup unless `FORCE_CONFIG=true`.

### 1.4 Key non-obvious behaviours

- The proxy resolves upstreams with its own resolver (`hickory-resolver`) using `UPSTREAM_DNS`, never the system resolver — in Kubernetes the pod's resolver may forward to the very DNS server that is intercepting CDN names.
- Upstream targets that resolve to private/loopback ranges are refused unless configured (FR-64).
- Client disconnects do not cancel slice fills (FR-31); fills are tasks owned by the store, not the connection.
- Backpressure: slice futures are awaited in order; the window bounds RAM per connection to `READAHEAD_SLICES × slice_size`.

## 2. Repository layout

Single workspace, one binary crate with modules, plus a dev-only test-kit crate; split further only when a module needs an independent release cadence.

```
cachic/
├── Cargo.toml                 # workspace
├── rust-toolchain.toml        # pinned stable
├── deny.toml                  # cargo-deny (licences, advisories, bans)
├── justfile                   # dev loop: fmt, lint, test, bench, image, chart
├── crates/
│   ├── cachic/            # the binary
│   │   └── src/
│   │       ├── main.rs        # CLI + wiring
│   │       ├── config/        # env + file, validation, units parsing
│   │       ├── services/      # cache-domains loader, matcher, key rules
│   │       ├── proxy/         # hyper server, router, headers, range parsing
│   │       ├── orchestrator/  # slice planning, pipeline, filler registry
│   │       ├── upstream/      # client, resolver, limits, retries
│   │       ├── store/         # foyer wrapper, slice codec, object index
│   │       ├── sni/           # 443 pass-through
│   │       ├── admin/         # metrics, health, purge, reload
│   │       └── telemetry/     # tracing, metrics, access log formats
│   └── cachic-testkit/    # mockcdn, load generator, trace replay (dev-dep)
├── charts/cachic/         # Helm chart
├── deploy/compose/            # docker compose examples (dns + cache; reference lancache for benchmarks)
├── deploy/flux/               # HelmRelease + OCIRepository example
├── docs/                      # mdBook: quickstart, config, k8s, migration, benchmarks, adr/
├── dashboards/                # Grafana JSON
├── .github/workflows/         # if canonical on GitHub
├── .gitlab-ci.yml             # if canonical on GitLab (or mirror CI)
├── Dockerfile
├── CONTRIBUTING.md  SECURITY.md  CODE_OF_CONDUCT.md  CHANGELOG.md  LICENSE  README.md
```

## 3. Library choices

| Area | Crate(s) | Notes |
|---|---|---|
| Runtime | `tokio` (multi-thread) | |
| HTTP server | `hyper` 1.x, `hyper-util`, `http`, `http-body-util`, `bytes` | HTTP/1.x only on the client side |
| Upstream client | `reqwest` (rustls, streaming) or `hyper-util` legacy client + `hyper-rustls` | Pool per host, HTTP/1.1, timeouts, no auto-redirect (handle explicitly) |
| DNS | `hickory-resolver` | Dedicated resolver, IPv4/IPv6 |
| TLS | `rustls`, `webpki-roots` | No OpenSSL dependency; static builds |
| Store | `foyer` (hybrid cache), `redb` (object index) | Pinned; isolated behind `store::Store` trait |
| Hashing | `blake3` (object ids), `xxhash-rust` (slice checksums) | |
| Config | `clap` (derive, env), `figment` or `config`, `serde`, `toml` | TOML for the optional rules file; YAML via `serde_yaml_ng` if users insist |
| Logging | `tracing`, `tracing-subscriber` (json) | Access log as a dedicated `tracing` target with its own formatter |
| Metrics | `metrics` + `metrics-exporter-prometheus` (or `prometheus-client`) | foyer exposes its own metrics via `metrics` |
| Admin HTTP | `axum` (tiny) | Same runtime, separate port |
| Concurrency | `dashmap`, `arc-swap` (hot config), `tokio::sync` | |
| SNI | `tls-parser` (ClientHello) + `tokio::io::copy_bidirectional` | |
| Allocator | `mimalloc` (feature-gated) | Measure vs system allocator in M0 |
| Testing | `cargo-nextest`, `proptest`, `cargo-fuzz`, `criterion`, `tempfile`, `testcontainers` (optional) | mockcdn lives in the testkit crate |

## 4. Phases and milestones

Estimates are calendar weeks at part-time effort; treat them as ±50 %.

### M0 — Spike and ADRs (weeks 1–2)

Deliverables:
- Throwaway prototype: hyper server + reqwest + foyer serving sliced GETs from a mock upstream.
- Measurements on real hardware (NUC NVMe; optionally Synology NFS over 10 GbE): foyer write/read throughput with 1 MiB entries, RAM per indexed entry at 1–10 M entries, recovery time for a 500 GB cache, direct-IO vs page-cache behaviour, allocator choice.
- Pingora vs hyper comparison note (decision 0.3).
- ADRs written: language, store, index, slice size and key scheme, config surface, repo hosting.
- Repo skeleton: layout above, CI green on lint/test, `justfile`, licence, README stub.

Exit criteria: measured hit throughput ≥ 8 Gbps on the NUC with 8 clients; foyer index memory per entry known; go/no-go on Rust confirmed.

### M1 — MVP proxy, v0.1 (weeks 3–6)

- Services: cache-domains loader (bundled snapshot), matcher, key normalisation with rules reproducing monolithic's per-service behaviour.
- Proxy: `GET`/`HEAD`, header handling, range parsing (single range), `200`/`206`/`416`, `X-Cache`, heartbeat endpoint, pass-through for other methods.
- Orchestrator: probe, slice plan, ordered pipeline with read-ahead window, foyer `fetch` dedup.
- Upstream: client pool, dedicated resolver, timeouts, single retry, private-address guard.
- Store: foyer wrapper, slice codec with checksum, redb index, config guard.
- Telemetry: JSON logs, core metrics, `/healthz`, `/readyz`.
- Packaging: Dockerfile (multi-stage, static musl, distroless/scratch), compose example with `lancache-dns`.

Exit criteria: SteamPrefill and Epic prefill complete through the proxy; differential tests pass; image runs as non-root on amd64 and arm64.

### M2 — Robustness, v0.2 (weeks 7–9)

- `no_ranges` path with object-level filler; validator-change → generation bump; stale-on-error; `If-Range`.
- Client-disconnect semantics; graceful shutdown; per-service and global concurrency limits.
- Admin API (stats, purge, reload, drain); `MIN_FREE_DISK` guard.
- Chaos suite: `kill -9` mid-fill, disk full, slow disk, flaky upstream; recovery assertions.
- Fuzzing of range and cache-domains parsers wired into CI (short runs).

Exit criteria: chaos suite green; 48-hour soak on the homelab with real clients.

### M3 — Deployment and parity, v0.3 (weeks 10–12)

- Helm chart (§7), Flux example, Kubernetes docs (LB IP, `externalTrafficPolicy: Local`, storage choices, `dnsConfig`).
- `lancache` access-log format; LANCache Manager smoke test.
- Benchmark harness and published report vs `lancachenet/monolithic` (§9).
- Release pipeline: signed multi-arch images, chart publishing, binaries, changelog.

Exit criteria: chart installed on the Talos cluster via Flux with ≤ 10 values; benchmark report shows parity on every scenario.

### M4 — v1.0 (weeks 13–16)

- SNI pass-through on 443; read-ahead tuning; domain-list auto-refresh with hot reload; Grafana dashboard.
- Per-service rule parity review against monolithic's config for every service in cache-domains.
- 7-day soak; docs complete (migration guide, config reference generated from the schema).
- Announce; open issues for 1.x backlog (revalidation, nginx cache import, OTel, block device, sharding).

## 5. Testing strategy

| Level | What | Tooling |
|---|---|---|
| Unit | Range parsing (property-based), key normalisation against cache-domains fixtures, slice arithmetic, header filtering, config precedence and unit parsing | `proptest`, fixtures in `testdata/` |
| Fuzz | `Range` header, `Content-Range`, cache-domains files, config file | `cargo-fuzz`; 5-minute runs in CI, longer nightly |
| Component | Orchestrator against `mockcdn`: range-capable, range-ignoring, flaky 5xx, slow, changing validators mid-object, redirects, chunked bodies, zero-length | in-process `mockcdn` on a random port, `#[tokio::test]` |
| Differential | For random URLs and random ranges, bytes through the proxy == bytes from `mockcdn` (deterministic content `f(url, offset)`); repeated with a warm cache | testkit `differ` |
| Integration | Built binary + `mockcdn` + `lancache-dns` in compose; load with a small Rust load generator (or `oha`); verify hashes and metrics | compose profile `ci` |
| Chaos | Crash mid-fill, disk full (small tmpfs), IO throttling via cgroups, DNS failure | compose profile `chaos`, scripted assertions |
| Performance | Micro: hashing, range parsing, slice codec (`criterion`); Macro: §9 harness | dedicated runner, results committed to `docs/benchmarks/` |
| Real-world (manual/nightly-optional) | SteamPrefill / Epic / Battle.net through the proxy on the homelab | needs credentials; documented, not required for CI |

Coverage is measured with `cargo-llvm-cov`; target ≥ 80 % on `services`, `orchestrator`, `store`, `proxy`.

## 6. CI/CD and release engineering

Canonical hosting is an open question (PRD §12). Both configurations below run the same `just` recipes so either can be the mirror.

**On every push / MR**: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo nextest run`, `cargo deny check` (licences: permissive only; advisories), `cargo audit`, typo check, docs build, chart lint (`ct lint`, `helm-docs` up to date), image build (no push).

**Nightly**: fuzz (30 min), chaos suite, integration load test, MSRV check, dependency-update PRs (Renovate).

**Tags (`vX.Y.Z`)**: `release-plz`/`cargo-release` bumps + `git-cliff` changelog (conventional commits); binaries via `cargo-dist` (linux amd64/arm64 musl, macOS); images via `docker buildx` — on GitLab use native runners on the cluster's amd64 and arm64 nodes and merge the manifest, avoiding QEMU; images to GHCR and/or GitLab registry (Harbor later), signed with `cosign` keyless, SBOM via `syft`; chart to an OCI registry (`ghcr.io/…/charts` or GitLab package registry) plus `chart-releaser` pages index; GitHub Release / GitLab Release with artefacts.

**Chart CI**: `ct lint` + `ct install` on a `kind` cluster, `helm unittest` for templates, `helm test` hook that curls the heartbeat endpoint.

**Repo hygiene**: CODEOWNERS, issue/MR templates, DCO sign-off, SemVer with a written stability policy for env vars and the admin API.

## 7. Packaging: image and Helm chart

### 7.1 Image

- Multi-stage build with `cargo-chef` for layer caching; `--release` with LTO and `codegen-units=1`; static musl binary; final stage `gcr.io/distroless/static` (or `scratch`) with a non-root user and `CAP_NET_BIND_SERVICE` documented for hosts that need port 80 without a port map.
- Tags: `vX.Y.Z`, `vX.Y`, `latest`, `sha-…`; OCI labels; SBOM attached.

### 7.2 Chart design

```yaml
image: { repository: ghcr.io/<org>/cachic, tag: "", pullPolicy: IfNotPresent }
replicaCount: 1                      # fixed; strategy: Recreate (RWO volume)
service:
  type: LoadBalancer                 # or ClusterIP + hostNetwork
  loadBalancerIP: ""                 # stable LAN IP the DNS server points at
  annotations: {}                    # MetalLB / Cilium LB-IPAM
  externalTrafficPolicy: Local       # keep client IPs for logs/metrics
  ports: { http: 80, https: 443 }
hostNetwork: false
cache:
  diskSize: 1000g
  memSize: 2g
  maxAge: 3560d
  sliceSize: 1m
  minFreeDisk: 10g
persistence:
  enabled: true
  storageClass: ""                   # see storage note
  size: 1100Gi                       # > diskSize + index + headroom
  existingClaim: ""
upstreamDns: ["1.1.1.1", "1.0.0.1"]
cacheDomains: { bundled: true, refresh: true, interval: 24h }
services: {}                         # per-service overrides → ConfigMap
logging: { format: json, level: info }
admin: { port: 9090, token: "" }
metrics:
  serviceMonitor: { enabled: false, labels: {} }
  grafanaDashboard: { enabled: false, labels: { grafana_dashboard: "1" } }
resources: { requests: { cpu: 500m, memory: 3Gi }, limits: { memory: 4Gi } }
nodeSelector: {}  tolerations: []  affinity: {}
dnsPolicy: ClusterFirst  dnsConfig: {}   # app resolver is used for upstreams regardless
securityContext: { runAsNonRoot: true, readOnlyRootFilesystem: true, capabilities: { add: [NET_BIND_SERVICE] } }
```

Storage note for the Talos/Longhorn cluster: a replicated Longhorn volume is the wrong shape for a cache (replication cost, throughput ceiling). Prefer, in order: a local PV / `hostPath` on the node with fast disks (pin with `nodeSelector`); a Longhorn StorageClass with `numberOfReplicas: "1"` and strict-local data locality; an NFS/iSCSI PV from the Synology over 10 GbE. The chart supports all three through `persistence.storageClass`/`existingClaim` plus affinity.

### 7.3 Flux example

`deploy/flux/`: `OCIRepository` pointing at the chart OCI ref, `HelmRelease` with the values above, `values` for the LB IP and storage class, and a `Kustomization` that also installs the ServiceMonitor. Secrets are unnecessary unless an admin token is set (then via External Secrets).

## 8. Documentation

- mdBook in `docs/`, published with Pages; sections: quickstart (compose), Kubernetes (Helm + Flux), configuration reference (generated from the config schema with `clap`'s help + a small script), service rules, migration from lancache (env mapping, what carries over, what does not), observability (metrics catalogue, dashboard), benchmarks, architecture, ADRs (`docs/adr/NNNN-*.md`, MADR format), contributing.
- README: what it is, 30-second compose start, links.
- `ARCHITECTURE.md` mirrors §1 and stays current (checked in review).

## 9. Benchmark protocol (parity with monolithic)

Environment: one amd64 NUC (NVMe) as cache host, second host as 10 GbE client, `mockcdn` on a third host or container behind `tc netem` (1 Gbps, 20 ms) to emulate WAN. Same data volume mounted into `lancachenet/monolithic` and `cachic` in alternating runs; identical `cache-domains`.

| Scenario | Measures |
|---|---|
| S1 warm single client, full 20 GB object | Gbps, CPU %, RSS |
| S2 warm 32 clients, same object | aggregate Gbps, p50/p99 TTFB, CPU per Gbps |
| S3 warm 32 clients, 32 distinct objects | same, plus disk IOPS/MB/s |
| S4 cold fill, 8 clients, same object | upstream bytes (should ≈ object size once), client Gbps |
| S5 random 64 KiB–8 MiB ranges into 5 GB objects (WSUS-like) | hit ratio, upstream amplification, p99 |
| S6 restart with 500 GB cached | time to first hit, time to full index |
| S7 eviction at cap, 24 h mixed replay | hit ratio, eviction rate, latency stability |

Report includes hardware, versions, raw CSV and the commands, committed under `docs/benchmarks/`.

## 10. Engineering risks and mitigations

| Risk | Mitigation |
|---|---|
| foyer API churn / entry-size assumptions differ from ours | Pin; `Store` trait boundary; M0 measurements; fallback design ready (per-object sparse files + bitmap, ~2 weeks) |
| Index memory at 10 M+ entries | Measure in M0; document `CACHE_MEM_SIZE`/index guidance like lancache does; prune by age |
| Hyper HTTP/1.x server edge cases with quirky game clients (HTTP/1.0, odd headers) | Capture real traffic samples early (Steam, WU, Blizzard) into fixtures; differential tests |
| Throughput shortfall vs nginx `sendfile` on HDD | Direct-IO + large sequential slice reads; io_uring engine; if still short, add a `sendfile` path for disk-tier hits |
| Rust contributor onboarding | `justfile`, devcontainer, ARCHITECTURE.md, "good first issue" rules work |

## Appendix A — Go alternative stack

If M0 rejects Rust, the equivalent plan in Go:

| Area | Choice |
|---|---|
| HTTP | `net/http` server; `http.Transport` upstream with `DialContext` using a custom `net.Resolver`; `sendfile` on disk-tier hits via `io.Copy` from `*os.File`/`io.SectionReader` |
| Store | Custom: per-object sparse file + bitmap sidecar or region log; `pebble`/`bbolt` index; `otter` or `ristretto` RAM tier; `x/sync/singleflight` for dedup |
| Config / CLI | `koanf` or `caarlos0/env`, `cobra` optional |
| Telemetry | `log/slog` JSON, `prometheus/client_golang`, OTel SDK |
| Layout | `cmd/cachic`, `internal/{config,services,proxy,orchestrator,upstream,store,sni,admin,telemetry}`, `charts/`, `deploy/`, `docs/` |
| CI / release | `golangci-lint`, `go test -race -fuzz`, `goreleaser` (binaries + multi-arch images), `ko` optional, same chart pipeline |

Cost delta: +3–5 weeks for the store, −1–2 weeks elsewhere; Windows becomes a first-class host.

## Appendix B — ADR list (to write in M0)

1. Language and runtime (Rust/tokio)
2. HTTP layer (hyper vs Pingora)
3. Store engine (foyer) and object index (redb)
4. Slice size, key scheme, generation semantics
5. Configuration surface and lancache env compatibility
6. Repository hosting and CI topology
7. Access-log compatibility with lancache tooling
8. Security posture: allow-listed upstreams, no TLS termination

## Appendix C — Definition of done for v1.0

- All PRD P0 and P1 requirements implemented with tests.
- Benchmark report published showing parity on S1–S7.
- Helm chart installed via Flux on the Talos cluster; Grafana dashboard live.
- SteamPrefill, Epic and Battle.net prefill runs complete; LANCache Manager log features working.
- 7-day soak with zero integrity failures.
- Docs site complete; CHANGELOG and signed release artefacts for amd64 and arm64.