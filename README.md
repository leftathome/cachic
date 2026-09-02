# cachic

An HTTP caching proxy for game-distribution and OS-update CDN traffic. It replaces the nginx
engine inside `lancachenet/monolithic` while keeping lancache's deployment model: a DNS server
answers CDN hostnames with the cache's IP, clients speak plain HTTP to the cache, and the cache
fetches, slices, stores and serves content that is effectively immutable.

What changes is that the cache is a purpose-built application rather than generated nginx config:

- **Slice-aware range caching.** Requests are served from fixed-size aligned slices, fetched from
  the origin only as needed, and streamed to the client in order as they arrive.
- **Request coalescing that streams.** Thirty clients starting the same download produce one
  upstream fetch per slice, and all of them stream the fill in progress rather than blocking on a
  lock.
- **A hybrid RAM + disk store with bounded memory.** Both tiers are hard caps, and per-connection
  memory is `READAHEAD_SLICES × CACHE_SLICE_SIZE` by construction.
- **Observability from day one.** Prometheus `/metrics`, structured JSON logs, liveness and
  readiness probes, and an admin API — rather than an access log on a volume for dashboards to
  tail.
- **12-factor configuration.** Environment variables named in cache terms, not nginx terms.
- **Kubernetes-native delivery.** A multi-arch image and a Helm chart that installs on a
  Talos/Flux cluster from a handful of values.

## Status

**Feature-complete against the plan; unreleased and unproven in the field.** The proxy serves
cached content end to end, with SNI pass-through on 443, an admin API, metrics, health probes,
automatic domain-list refresh and a Grafana dashboard. It is packaged as a container and a Helm
chart, and a release pipeline exists but has never been triggered.

What has *not* happened: no benchmark against `lancachenet/monolithic`, no run against real game
clients, no 7-day soak, no tagged release. Six items in the
[definition of done](docs/definition-of-done.md) are blocked on reference hardware rather than on
code. Do not point a LAN party at it yet.

See [the task index](.agent/tasks/TASK-INDEX.md) for the milestone-by-milestone position.

## Quickstart

### docker compose

```sh
cd deploy/compose
export LANCACHE_IP=192.168.1.10        # this host's LAN address
docker compose up -d
```

Then point your clients' DNS at `LANCACHE_IP`. Nothing on the client changes. See
[deploy/compose/README.md](deploy/compose/README.md).

### Kubernetes

```sh
helm install cachic oci://ghcr.io/leftathome/charts/cachic \
  --set service.loadBalancerIP=192.168.1.10 \
  --set persistence.storageClass=local-path \
  --set persistence.size=2200Gi
```

See the [chart README](charts/cachic/README.md), the [Kubernetes guide](docs/kubernetes.md), and
a [Flux example](deploy/flux/README.md).

### Checking it works

```sh
curl -i -H 'Host: lancache.steamcontent.com' http://LANCACHE_IP/lancache-heartbeat
```

A `204` with `X-LanCache-Processed-By` means prefill tools and LANCache Manager will detect the
cache.

## Documentation

| | |
|---|---|
| [Configuration reference](docs/configuration.md) | Every setting, generated from the code |
| [Kubernetes guide](docs/kubernetes.md) | Probes, storage, sizing, what to watch |
| [Helm chart](charts/cachic/README.md) | Values, and why a replicated volume is the wrong shape |
| [Flux example](deploy/flux/README.md) | GitOps deployment |
| [Architecture decisions](docs/adr/) | Why it is built this way, and what would change each answer |
| [M0 measurements](docs/benchmarks/m0/README.md) | Throughput, index cost, and a benchmark that lied |
| [Per-service parity](docs/service-parity.md) | Reviewed against monolithic's nginx config |
| [Definition of done](docs/definition-of-done.md) | What is verified, and what needs hardware |
| [Migrating from lancache](docs/migration-from-lancache.md) | What carries over, and what does not |
| [PRD](docs/cachic-PRD.md) / [plan](docs/cachic-IMPLEMENTATION-PLAN.md) | Requirements and milestones |

## Two things worth knowing before you deploy it

**Watch `foyer_storage_inner_op_total{op="channel_overflow"}`.** If it is climbing, writes are
outrunning the disk and the cache is silently declining to store content. That is this product's
worst failure mode — clients still get their bytes at full speed, the cache just never warms — and
it is invisible without this counter.

**`UPSTREAM_DNS` must not be your lancache DNS server.** That server answers CDN hostnames with
the cache's own address, so resolving through it would loop every fetch back into the cache. This
is safe by construction rather than by configuration: the constructors that read `/etc/resolv.conf`
are not compiled into the binary.

## Building

```sh
just check     # fmt, clippy, cargo-deny, typos, tests, and the performance gate
just test
just image
```

The performance gate enforces the project's floor standard — as good and fast as nginx, but easier
to configure and operate — as a build failure rather than an aspiration. See
[ADR 0009](docs/adr/0009-performance-floor.md).

### WSL2 note

If this repository lives on a Windows drive (`/mnt/c/...`), point Cargo's target directory at
native Linux storage. Building and benchmarking through the DrvFs bridge measures the filesystem
bridge, not the code:

```sh
export CARGO_TARGET_DIR=~/.cache/cachic-target
```

The same applies to any cache data directory used for benchmarks.

## Licence

[Apache License 2.0](LICENSE).
