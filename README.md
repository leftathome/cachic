# cachic

An HTTP caching proxy for game-distribution and OS-update CDN traffic. It replaces the nginx
engine inside `lancachenet/monolithic` while keeping lancache's deployment model: a DNS server
answers CDN hostnames with the cache's IP, clients speak plain HTTP to the cache, and the cache
fetches, slices, stores and serves content that is effectively immutable.

What changes is that the cache is a purpose-built application rather than generated nginx config:
slice-aware range caching, a hybrid RAM + disk store with bounded memory, Prometheus metrics,
structured logs, health probes, an admin API, 12-factor configuration, multi-arch OCI images and
a Helm chart.

## Status

**Pre-alpha.** M0 (spike, measurements and ADRs) is in progress. The binary does not proxy
anything yet.

- [Product requirements](docs/cachic-PRD.md)
- [Implementation plan](docs/cachic-IMPLEMENTATION-PLAN.md)
- [Task index](.agent/tasks/TASK-INDEX.md)
- [Architecture decisions](docs/adr/)

## Building

```sh
cargo build
just lint
just test
```

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
