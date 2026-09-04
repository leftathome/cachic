# Configuration reference

Generated from the command-line definitions; do not edit by hand. Regenerate with:

```sh
cargo run --example config-reference > docs/configuration.md
```

Every setting can be given as an environment variable or as a command-line flag. Precedence is environment > file > defaults.

Sizes accept nginx spellings (`1000g`, `2g`, `1m`) which are binary multiples, matching `lancachenet/monolithic`, as well as IEC (`GiB`) and SI (`GB`, decimal). Durations accept `s`, `m`, `h`, `d`, `w`.

| Environment variable | Flag | Default | Description |
|---|---|---|---|
| `CACHE_DISK_SIZE` | `--cache-disk-size` | `1000g` | Disk tier capacity. Hard cap (FR-40) |
| `CACHE_MEM_SIZE` | `--cache-mem-size` | `2g` | RAM tier capacity for hot slices. Hard cap. Index memory is reported separately and is roughly 400 bytes per stored slice; see the sizing guide |
| `CACHE_MAX_AGE` | `--cache-max-age` | `3560d` | How long an object stays cacheable |
| `CACHE_SLICE_SIZE` | `--cache-slice-size` | `1m` | Slice size. Persisted with the cache; changing it requires FORCE_CONFIG=true (FR-10) |
| `CACHE_DIRECT_IO` | `--cache-direct-io` | `false` | Bypass the page cache for disk-tier I/O.  Off by default, which means every slice written to disk is also held in the kernel's page cache. That is double caching - cachic already has its own RAM tier - and under a cgroup limit it counts toward the container's working set, so memory use looks like it is climbing without bound when most of it is reclaimable. Turn this on to make the RAM tier the only place slices are cached in memory, at the cost of losing the kernel's read caching for anything the RAM tier misses. Linux only; ignored elsewhere. |
| `CACHE_FLUSHERS` | `--cache-flushers` | `4` | Disk-tier flush threads.  foyer drops a disk write when its flushers cannot keep up, so this sets the write rate the cache can absorb before it starts silently losing slices. The default handles a 5 Gbit fill; raise it with the buffer pool for 10 Gbit and above. |
| `CACHE_BUFFER_POOL` | `--cache-buffer-pool` | `128m` | Disk-tier flush buffer pool, shared across flushers |
| `CACHE_DATA_DIR` | `--cache-data-dir` | `/data/cache` | Cache data directory |
| `MIN_FREE_DISK` | `--min-free-disk` | `10g` | Reduce the effective disk cap when the filesystem falls below this much free space (FR-46) |
| `FORCE_CONFIG` | `--force-config` | `false` | Adopt the current settings even though they disagree with the cache directory. Existing slices become unreachable |
| `HTTP_PORT` | `--http-port` | `80` |  |
| `HTTPS_PORT` | `--https-port` | `443` |  |
| `ADMIN_PORT` | `--admin-port` | `9090` |  |
| `UPSTREAM_DNS` | `--upstream-dns` | `1.1.1.1 1.0.0.1` | Resolvers used for upstream lookups. Never the system resolver: in a lancache deployment the system resolver is the one lying about CDN hostnames, and using it loops traffic back into this cache (FR-03) |
| `UPSTREAM_MAX_INFLIGHT` | `--upstream-max-inflight` | `256` | Global cap on concurrent upstream fetches |
| `STALE_ON_ERROR` | `--stale-on-error` | `true` | On an upstream 5xx or timeout, serve whatever slices are already cached and fail only the missing ones (FR-22).  On by default. During a CDN outage this is the difference between a client that gets the part of the object we hold and retries for the rest, and one that gets nothing at all. Turn it off if a partial response is worse for your clients than a failed one. |
| `READAHEAD_SLICES` | `--readahead-slices` | `4` | Prefetch this many slices ahead on sequential reads. Per-connection memory is this multiplied by the slice size (FR-16) |
| `ALLOW_PRIVATE_UPSTREAMS` | `--allow-private-upstreams` | `false` | Allow upstream fetches to private, loopback and link-local addresses.  Off by default and should stay that way: without the guard, anyone on the LAN can point the cache at a router's admin interface or a cloud metadata endpoint and have it fetch and serve the result (FR-64). Turn it on only to cache from a deliberate internal mirror. |
| `PASSTHROUGH_UNKNOWN_HOSTS` | `--passthrough-unknown-hosts` | `false` | Proxy hosts that match no service, instead of returning 404. Off by default: with it on and no allow-list, the cache is an open proxy on the LAN (FR-64) |
| `CACHE_DOMAINS_REPO` | `--cache-domains-repo` | `https://github.com/uklans/cache-domains` |  |
| `CACHE_DOMAINS_REFRESH` | `--cache-domains-refresh` | `24h` | How often to refresh the domain list. Zero disables refresh, for air-gapped installs |
| `CACHE_DOMAINS_DIR` | `--cache-domains-dir` | - | Load the domain list from this directory instead of the bundled snapshot.  The directory must be laid out like `uklans/cache-domains`: a `cache_domains.json` naming each service and the `.txt` files listing its hostnames. Useful for a custom service, for an air-gapped site pinning its own copy, and for testing against a local origin. |
| `CACHE_RULES_FILE` | `--rules-file` | - | Optional TOML file of per-service rules |
| `LOG_FORMAT` | `--log-format` | `json` |  |
| `LOG_LEVEL` | `--log-level` | `info` |  |
| `ADMIN_TOKEN` | `--admin-token` | - | Bearer token for the admin API. Empty means unauthenticated, which is only safe because the admin port is bound to loopback or a cluster network by default (FR-54) |

## Sizing notes

- **Per-connection memory** is `READAHEAD_SLICES * CACHE_SLICE_SIZE`. At the defaults that is 4 MiB per in-flight client request, and it is a hard bound rather than a target.
- **Index memory** is roughly 400 bytes per stored slice, and is *not* part of `CACHE_MEM_SIZE`. A 2 TB cache at 1 MiB slices needs about 760 MB for the index; at 4 MiB slices, about 190 MB. `lancachenet/monolithic`'s equivalent figure is around 128 bytes per slice, so budget roughly three times what you would have under nginx.
- **Changing `CACHE_SLICE_SIZE`** against an existing cache directory aborts startup. The stored slices were written under the old size and cannot be reinterpreted. Set `FORCE_CONFIG=true` to adopt the new size and abandon the existing cache.
