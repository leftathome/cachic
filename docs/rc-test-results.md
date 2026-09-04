# RC test results — v0.1.0-rc1

Execution of [`rc-test-plan.md`](rc-test-plan.md) against the published release
artefacts, 2026-09-02/03, on a Talos/Flux homelab cluster. Parity numbers and
their caveats: [`benchmarks/orac/`](benchmarks/orac/README.md).

Everything here was run against **released artefacts** — the published image and
the published chart — deployed the way §B asks, via Flux with a pinned
`OCIRepository`. No local build.

## Verdict

**Do not ship 0.1.0-rc1 as-is.** Three defects are individually blocking, and two
of them stop the documented install from working at all:

- **[F7](#f7)** — the chart sets no `fsGroup`, so the pod cannot write its own
  cache directory on a freshly provisioned volume and CrashLoops on first
  install.
- **[F11](#f11)** — the shipped memory defaults are OOMKilled under 16 concurrent
  clients, half the load this plan's own benchmark protocol specifies.
- **[F10](#f10)** — `UPSTREAM_DNS` is consulted but does not determine where
  upstream fetches connect; the system resolver does. That is precisely the
  failure FR-03 exists to prevent.

None is subtle once looked for, and all three are invisible to a test suite that
never puts the chart on a real cluster with real block storage and real
concurrency.

**The engine itself looks good.** Zero integrity failures in 41 checked
transfers, perfect upstream coalescing, 397 ms index recovery on a populated
cache, cache surviving pod replacement with zero refill, and cold-fill
time-to-first-byte **15x better than monolithic**. The problems are in packaging,
defaults, and one wiring bug — not in the caching.

## Section status

| Section | Status |
|---|---|
| A — parity vs monolithic | **partial.** S1–S5 on both engines, S6 on cachic, using a rig we had to build (F2, F3). S7 not run. Throughput was link-bound, so §A3's 5% bar is not settled here. |
| B — Kubernetes deployment | **run; fails on the shipped chart** (F7, F11). Passes with two patches. |
| C — client & ecosystem | **not run.** Needs CDN hostnames redirected at the site's LAN resolver — a household-wide network change. |
| D — seven-day soak | **not run.** Needs the harness from F2, and conflicts with occupying the one host both engines must share. |
| E — fill-rate validation | **not run.** Needs the harness from F2. Note `UPSTREAM_MAX_INFLIGHT`, which §E asks be tuned, is unreachable through the chart (F9). |

---

## Blocking

### F7 — the chart cannot start on a fresh PVC: no `fsGroup`, non-root user {#f7}

Deploying the chart exactly as documented produces an immediate CrashLoop:

```
configuration error: cannot write /data/cache/CONFIG: Permission denied (os error 13)
  caused by: Permission denied (os error 13)
```

The pod runs as UID 65532 with `runAsNonRoot: true`, but **`fsGroup` appears
nowhere in the chart** — there is not even a value to set.
`templates/deployment-cachic.yaml` emits a pod securityContext containing only
`runAsNonRoot` and `runAsUser`.

A dynamically provisioned block volume is formatted and mounted `root:root 0755`.
Without `fsGroup`, kubelet never adjusts ownership and the non-root process
cannot create its state file in an empty cache directory. **This is not
Longhorn-specific** — Ceph RBD, AWS EBS and GCE PD all behave the same way. Any
first install onto dynamically provisioned block storage fails.

This contradicts G6/FR-71 directly, and it is the claim §B4 exists to check: the
install does not need *more than ten* values, it needs a value the chart does not
expose. Only a storage class handing out world-writable directories (some
`hostPath`/local-path provisioners) would mask it, which is plausibly why it
survived development.

**Fix:** add `fsGroup` to the pod securityContext defaulting to the same UID
(65532), ideally with `fsGroupChangePolicy: OnRootMismatch` so a large existing
cache is not recursively re-chowned on every restart. Adding exactly that took
the pod from CrashLoop to ready with no other change.

### F10 — upstream fetches connect via the system resolver, not `UPSTREAM_DNS` {#f10}

cachic queries `UPSTREAM_DNS` and then connects to the address the **system
resolver** returned. The setting is consulted but does not determine where the
traffic goes.

Established with two resolvers that disagree: a test resolver answering
`lancache.steamcontent.com` with a mock origin, and the system resolver answering
with the real Steam CDN. Only DNS configuration changed between runs.

| `UPSTREAM_DNS` | system resolver | result |
|---|---|---|
| blackholed (192.0.2.1) | working | `resolving "lancache.steamcontent.com" failed: request timed out` — so the setting **is** consulted, and is on the critical path |
| → mock origin | → real Steam | **fetched the real Steam CDN** (403); the mock origin logged nothing |
| → mock origin | blackholed | `error sending request for url (...)` — cannot connect at all, despite `UPSTREAM_DNS` resolving fine |
| → mock origin | → mock origin | 206, 1 048 576 bytes; mock origin logged the request |

The test resolver logged cachic's `A` and `AAAA` queries in every case and
answered them with the mock origin's address. This is not a "cachic never asked"
failure — it asked, got the right answer, and connected somewhere else. The last
row is the only configuration in which we could steer an upstream fetch at all,
and it works by overriding the **pod's** resolver.

**Why this blocks.** `UPSTREAM_DNS` exists for one reason, which the binary's own
help text states:

> Resolvers used for upstream lookups. Never the system resolver: in a lancache
> deployment the system resolver is the one lying about CDN hostnames, and using
> it loops traffic back into this cache (FR-03)

In the canonical lancache deployment the host's resolver **is** the lancache DNS.
A cache that resolves CDN hostnames through it resolves them to itself and loops
every upstream fetch back into its own listener. The guard is present, documented
and configurable — and does not control the connection.

It also falsifies the claim in `values.yaml`:

> The app resolves upstreams through upstreamDns regardless of this, so cluster
> DNS settings cannot reintroduce the resolution loop.

Cluster DNS settings were the only thing that changed the outcome.

This was invisible on Kubernetes until we looked for it, because `dnsPolicy:
ClusterFirst` points at CoreDNS, which is not a lancache DNS and resolves CDN
names correctly to the real internet. The cache appears to work. It is the Docker
and bare-metal deployments — the primary ones — where the system resolver is most
likely to be the lancache DNS and the loop bites.

**Suggested regression test:** resolve one hostname to two different addresses
through the two resolvers and assert which one is dialled. That is the assertion
no current test makes.

### F11 — the shipped defaults OOM under 16 clients, mid-benchmark {#f11}

Running this plan's own scenarios against the chart's **unmodified** memory
settings (`cache.memSize: 2g`, `resources.limits.memory: 4Gi`), cachic was
OOMKilled during S3:

```
lastState: terminated  exitCode: 137  reason: OOMKilled
```

Container working-set memory over the run:

| time (UTC) | working set |
|---|---|
| 07:03:20 — idle, one 1 GiB object cached | 1069 MiB |
| 07:07:29 — benchmark starts | |
| 07:09:20 | 2296 MiB |
| 07:11:20 | 2464 MiB |
| 07:13:20 | 2999 MiB |
| 07:15:20 | 3136 MiB |
| 07:17:20 | 4084 MiB — **OOMKilled** |

Monotonic across ten minutes, no plateau, until the limit. The RAM tier was
configured at **2g** and the process reached roughly **4g**. Reproduced on a
later clean run: **4973 MiB** after 32 GiB of traffic.

The chart's own sizing guidance does not predict this:

> `resources.requests.memory` should cover `cache.memSize` plus the index plus a
> baseline … roughly 400 bytes per stored slice

For a 40g cache at 1m slices the index is ~40960 slices x 400 B ≈ 16 MiB, and 16
concurrent connections at `readaheadSlices 4 * sliceSize 1m` is a further 64 MiB.
The formula says ~2.1 GiB; observed 4 GiB and still climbing when killed.

Load was **16** concurrent clients. The benchmark protocol specifies **32**. The
shipped configuration therefore cannot complete this plan's own S3, let alone
S7's 24-hour replay — and §D2 independently lists "RSS growing steadily across
the window" as a soak failure criterion.

**Two consequences worth stating so nobody chases a phantom.** First, the S3
integrity failures (16/16) and the S4/S5 connection refusals in that same run
were *caused by this OOM kill*, not by independent cache-corruption or
availability bugs — the process died mid-transfer and the listener went away. With
the limit raised, the identical scenarios ran clean. Second, clients received
**truncated objects under a 200 status**. Content-Length made that detectable
here, but a client trusting the status code alone would have silently written a
short file.

---

## Non-blocking

### F2, F3 — the sections that produce numbers cannot be run from the release

§A2 gives `cachic-bench --scenario all …` and §D1 gives `cachic-soak --seconds
604800 …` as if they were executables. Neither ships:

- the release tarball contains exactly one file, `cachic`;
- the image contains exactly one binary, `/usr/local/bin/cachic`;
- `cachic --help` exposes no subcommands.

They are cargo **examples** (`crates/cachic/examples/bench.rs`, `soak.rs`),
reachable only as `cargo run --release --example bench` from a source checkout
with a Rust toolchain. Sections A, D and E — the ones that produce numbers — are
not executable from the released artefacts at all.

§A is worse than a packaging gap. `bench.rs` says in its own header that it
drives cachic and *"does not drive `lancachenet/monolithic`"*, and
`docs/benchmarks/README.md` confirms the parity report is unwritten. So §A asks
the tester to supply the entire comparison harness while stating pass/fail
criteria to five-percent precision against it. Two engines measured by two
different harnesses cannot be compared to 5%.

**Fix:** ship the harness (a second binary, or a `cachic bench` subcommand), or
have the plan say plainly that those sections need a source checkout and give the
cargo invocation. For §A, an engine-agnostic driver is very achievable — both
engines are plain HTTP proxies keyed on `Host` — and one is described in
[`benchmarks/orac/`](benchmarks/orac/README.md).

### F9 — the chart exposes 15 of the binary's 21 options, with no escape hatch

`templates/deployment-cachic.yaml` emits a fixed env list. These documented
options have no value and no way to reach the container — there is no
`extraEnv`, no `envFrom`, no free-form `env`:

| Unreachable | Why it matters |
|---|---|
| **`FORCE_CONFIG`** | see below |
| `ALLOW_PRIVATE_UPSTREAMS` | required to cache from "a deliberate internal mirror", which the help text names as the supported use case |
| `PASSTHROUGH_UNKNOWN_HOSTS` | |
| `STALE_ON_ERROR` | the CDN-outage behaviour; documented as tunable, is not |
| `UPSTREAM_MAX_INFLIGHT` | a fill-rate knob §E explicitly asks be adjusted |
| `CACHE_DOMAINS_REPO` / `_REFRESH` / `_DIR` | `_REFRESH=0` is the documented air-gapped install, unreachable on Kubernetes |

`FORCE_CONFIG` is the sharp edge, and **§B3 walks straight into it.** That step
has the tester change `cache.sliceSize` and confirm the refusal. The refusal
works and the message is genuinely good:

```
configuration error: cache directory was created with slice size 1 MiB (format version 2),
but this process is configured with slice size 2 MiB (format version 2).
The slices already stored there were written under the old setting and cannot be reinterpreted.
Either restore the previous setting, point CACHE_DATA_DIR at an empty directory,
or set FORCE_CONFIG=true to discard the existing cache.
```

It names both values, the format version, and three ways out. But **two of those
three cannot be applied through the chart** — neither `FORCE_CONFIG` nor
`CACHE_DATA_DIR` is exposed. A Kubernetes operator following this message has
exactly one option of the three, and the others are deleting the PVC or
hand-patching the Deployment behind Helm's back.

**Fix:** a single `extraEnv: []` passed through to the container closes the whole
table. `FORCE_CONFIG` additionally deserves a first-class value, since the chart
already models `cache.sliceSize` and therefore owns the change that triggers the
error.

### F8 — the `service` metric label collides with Prometheus's own

cachic labels its metrics with `service` (steam, blizzard, …).
kube-prometheus-stack's ServiceMonitor pipeline also attaches a `service` label —
the Kubernetes Service name — and on collision Prometheus keeps its own and
renames the exporter's to `exported_service`. Observed on a live scrape of the
chart's own ServiceMonitor:

```
cachic_requests_total{job="cachic-admin", service="cachic-admin",
                      exported_service="-", pod="cachic-699c4cf59f-254vh"}
```

The bundled dashboard queries `sum by (service) (…)` with `legendFormat:
"{{service}}"`. Grouped by the Kubernetes Service name, every per-service panel
collapses to a single flat series labelled `cachic-admin`, and the per-CDN
breakdown — the entire point of those panels — never appears. This follows from
the chart's own ServiceMonitor plus a stock kube-prometheus-stack, which is
exactly the combination §B2 asks the tester to verify.

**Fix:** rename the metric label to something that cannot collide (`cdn_service`,
`cache_service`); `service` is effectively reserved in a Kubernetes monitoring
stack. `honorLabels: true`, or having the dashboard query `exported_service`, are
both worse — they bake the collision into the contract.

### F6 — four dashboard panels give two queries the same `refId`

`dashboards/cachic.json` gives two targets the same `refId` in four of its nine
panels:

| Panel | Title | refIds |
|---|---|---|
| 2 | Bytes served vs fetched | `A`, `A` |
| 3 | **Silently dropped disk writes** | `A`, `A` |
| 6 | Upstream fetch latency | `A`, `A` |
| 7 | In-flight fetches and connections | `A`, `A` |

Grafana requires refIds unique within a panel; two targets sharing one collide,
and the panel typically renders a single series instead of both. Every affected
panel is a two-series comparison where the comparison *is* the point — panel 2 is
the value-of-the-cache panel, and panel 3 is the
`foyer_storage_inner_op_total{op="channel_overflow"}` panel that §D2 and §E2 both
hang their pass/fail criteria on. §B2 asks the tester to confirm the dashboard is
"fully populated", which this prevents.

**Fix:** one character per panel — the second target becomes `B`.

### F1 — the release binary requires GLIBC_2.38, and nothing says so

`cachic-v0.1.0-rc1-x86_64-unknown-linux-gnu.tar.gz` fails to execute on a host
with glibc 2.35:

```
./cachic: /lib/x86_64-linux-gnu/libc.so.6: version `GLIBC_2.38' not found (required by ./cachic)
```

GLIBC_2.38 lands in Ubuntu 24.04 (2.39) and Debian 13 (2.41). The binary
therefore does not run on **Ubuntu 22.04 LTS (2.35)** or **Debian 12 (2.36)**,
both ordinary homelab hosts and both still in support. Neither the release page,
the README, nor the test plan states a minimum glibc, and no musl/static target
is published.

**Fix:** ship an `x86_64-unknown-linux-musl` artefact, or state the floor on the
release page.

### F4 — parity is structurally impossible on arm64

`lancachenet/monolithic` publishes **amd64 only** — verified by digest equality
across `crane digest --platform linux/amd64` and `linux/arm64`, so the platform
flag is a no-op. cachic ships arm64 and the plan is billed as
environment-agnostic, but on an arm64-only deployment §A cannot be run at all.
Worth one sentence in the prerequisites.

### F5 — `helm test` pulls an unpinned Docker Hub image

`templates/tests/test-heartbeat.yaml` uses `curlimages/curl:latest`. That makes
`helm test` dependent on Docker Hub reachability and anonymous rate limits, and
an unpinned `:latest` in a release artefact is a reproducibility hole. On our
cluster it also lands on a known-broken cold-pull path through the registry
mirror.

**Fix:** pin by digest, or reuse the cachic image itself for the probe and drop
the dependency.

---

## What passed

| Check | Result |
|---|---|
| Image is genuinely multi-arch with attestations | pass — amd64 + arm64 |
| Heartbeat contract — 204 + `X-LanCache-Processed-By` | pass |
| Client source addresses in logs (`externalTrafficPolicy: Local`) | pass — real LAN address logged |
| Metrics collection — ServiceMonitor scraped, target `up` | pass — 22 metric families |
| `foyer_storage_inner_op_total{op="channel_overflow"}` present | pass |
| Startup with existing cache data | pass — 397.5 ms for 639 blocks / 21569 entries |
| Cache survives pod replacement without refill | pass — origin request count 1024 before, 1024 after |
| Slice-size mismatch refused with a clear message | pass — but see F9 |
| Integrity across all measured transfers | pass — 0 failures in 41 checks |
| Pod reaches Ready on the shipped chart | **fail — F7** |
| Grafana dashboard fully populated | **fail — F6, F8** |

## Reproducing

Deployed via Flux with a pinned `OCIRepository` and eight values, patched only
for F7 (`fsGroup`) and F11 (memory limit) — without those two the deployment does
not run at all. The benchmark rig, driver and raw CSV live in the operator's
gitops repo under `docs/recipes/cachic-rc1-bench/`; the parity write-up and its
caveats are in [`benchmarks/orac/`](benchmarks/orac/README.md).
