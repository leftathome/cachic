# cachic — Product Requirements Document

| | |
|---|---|
| **Status** | v0.1 |
| **Date** | 2026-09-01 |
| **Owner** | Steven Wagner / Apocryphal Information Systems |
| **Working name** | `cachic` |
| **Companion doc** | `cachic-IMPLEMENTATION-PLAN.md` |

## 1. Summary

`cachic` is a single-binary HTTP caching proxy for game-distribution and OS-update CDN traffic. It replaces the nginx engine inside `lancachenet/monolithic` while keeping lancache's deployment model unchanged: a DNS server answers CDN hostnames with the cache's IP, clients speak plain HTTP to the cache, and the cache fetches, slices, stores and serves content that is effectively immutable.

What changes is that the cache is a purpose-built application rather than generated nginx config: slice-aware range caching, a hybrid RAM + disk store with bounded memory, Prometheus metrics, structured logs, health probes, an admin API, 12-factor configuration, multi-arch OCI images and a Helm chart.

## 2. Problem statement

`lancachenet/monolithic` works well, but its engine is nginx plus shell-generated configuration, which shows in day-to-day operation:

- Tuning is expressed in nginx terms (`keys_zone` sizing, `slice`, `proxy_cache_lock`, loader parameters) rather than in cache terms (bytes on disk, bytes in RAM, slice size). The index lives in nginx shared memory sized by hand; the docs' rule of thumb is 1 MB of `CACHE_MEM_SIZE` per ~8 GB of 1 MB slices on disk.
- Observability is an access log on a volume. Every dashboard in the ecosystem (LANCache Manager, DeveLanCacheUI, lancache-ui) is a log tailer, and LANCache Manager additionally reads nginx's hashed cache directory to browse and purge.
- Coalescing of concurrent misses relies on `proxy_cache_lock` semantics (waiters block for the lock, they don't stream the in-flight fill).
- There is no admin API, no per-service logic beyond what nginx `map`s can express, and no place to put behaviour like read-ahead, integrity checks or graceful degradation.
- Kubernetes deployment is possible but awkward: no readiness signal that reflects cache state, no metrics endpoint, logs on a volume instead of stdout.

lancache's own FAQ explains why nginx was chosen: the ability to overwrite cache keys, cache for very long periods, rewrite upstream headers and exclude the hostname from the key. Those are ordinary application requirements; expressed in code they are small, testable and extensible.

## 3. Goals and non-goals

### Goals

| ID | Goal |
|---|---|
| G1 | **Functional parity** with `lancachenet/monolithic` for every service in `uklans/cache-domains` (Steam, Epic, Blizzard, Riot, Windows Update, Xbox, PlayStation, Nintendo, …). |
| G2 | **Performance parity or better** on the same hardware: cache hits at 10 GbE line rate from NVMe; disk-bound on HDD arrays; misses fill at WAN rate with ≤ 5 % overhead. |
| G3 | **12-factor operability**: env-var config, stdout logs, `/metrics`, liveness/readiness, graceful shutdown, one data volume, non-root container. |
| G4 | **Ecosystem compatibility**: works unchanged with `lancache-dns` and `cache-domains`; prefill tools (SteamPrefill, Epic/Battle.net/Riot prefill, LANCache Manager) detect it; optional lancache-format access log for existing dashboards. |
| G5 | **Small, contributor-friendly codebase** following the target language's community conventions for layout, CI, packaging, docs and tests. |
| G6 | **Kubernetes-native delivery**: multi-arch image plus a Helm chart that deploys cleanly on a Talos/Flux cluster with a handful of values. |

### Non-goals (v1)

| ID | Non-goal | Rationale |
|---|---|---|
| N1 | DNS interception | Provided by `lancache-dns`, Pi-hole, AdGuard, Unbound, etc. |
| N2 | TLS interception / caching HTTPS content | Requires MITM certificates on clients; out of scope. Port 443 is SNI pass-through only. |
| N3 | General RFC 9111 web-cache semantics (`Vary`, content negotiation, upstream-driven freshness) | This is an immutable-object cache with operator-defined TTLs, like lancache. |
| N4 | Multi-node clustering / consistent-hash sharding | Single node covers the target users; leave room in the design. |
| N5 | On-disk compatibility with nginx's cache directory | Different store. Offer an admin API instead and work with LANCache Manager upstream. |
| N6 | Web UI | LANCache Manager and friends already do this well. |
| N7 | Windows as a supported host platform | Linux amd64/arm64 containers are tier 1; macOS for development; Windows best-effort only. |

## 4. Users and scenarios

| Persona | Environment | What they need |
|---|---|---|
| Homelab operator (primary) | Kubernetes (Talos + Flux), Prometheus/Grafana, 10 GbE, mixed amd64/arm64 nodes | Helm install, metrics, sane defaults, small footprint |
| LAN-party admin | One beefy box, docker compose, HDD/SSD array, hundreds of clients | Throughput under concurrency, prefill support, fast restart |
| Small office / school | Windows Update + Steam, modest hardware | Reliability, low resource use, hands-off operation |
| Contributor | Wants to add a service rule or a feature | Clear architecture, tests, one-command dev loop |

Key scenarios the product must handle well:

1. **First download** — one client pulls a 60 GB game; the cache fills at WAN speed and the client sees no slowdown versus going direct.
2. **Second machine** — the same game is served at LAN wire speed from disk.
3. **LAN party burst** — 30 clients request the same content within seconds; upstream sees one fetch per slice, clients stream as slices land.
4. **Range-heavy clients** — Windows Update, Blizzard and Origin-style launchers issue many byte-range requests into multi-GB files.
5. **Prefill** — SteamPrefill/LANCache Manager warm the cache overnight.
6. **Cache full** — eviction keeps the cache at its size cap without stalls; hot content survives.
7. **Restart / crash** — the cache comes back serving hits within seconds; nothing is corrupted.
8. **Upgrade** — a new image is rolled out via Flux; the data volume is reused.

## 5. How it works (context)

```
clients ──(DNS: cdn.example → cache IP)──▶ :80  cachic ──(real DNS via dedicated resolver)──▶ upstream CDN (http/https)
                                            :443 SNI pass-through ───────────────────────────────▶ upstream :443 (uncached)
```

Vocabulary used throughout:

- **Service** — a named CDN family from `cache-domains` (e.g. `steam`, `blizzard`, `wsus`) with a hostname list and optional rules.
- **Cache identifier** — the service name; lancache uses it as the key prefix so identical content from any of a service's hosts shares one entry.
- **Object** — one logical URL after key normalisation (identifier + path, query string dropped unless a rule keeps it).
- **Slice** — a fixed-size aligned byte range of an object (default 1 MiB, matching lancache's `CACHE_SLICE_SIZE`). Slices are the unit of storage, fetch, coalescing and eviction.
- **Generation** — a counter bumped when an object's validators (ETag / Last-Modified / length) change; slices are keyed by generation so stale slices become unreachable instead of being served.

## 6. Functional requirements

Priority: **P0** = required for v1.0, **P1** = strongly desired for v1.0, **P2** = post-1.0.

### 6.1 Proxying

| ID | P | Requirement |
|---|---|---|
| FR-01 | P0 | Listen on a configurable HTTP port (default 80) and accept requests for **any** `Host`; HTTP/1.1 and HTTP/1.0 clients, keep-alive, thousands of concurrent connections. |
| FR-02 | P0 | Match `Host` against the `cache-domains` list (exact and wildcard entries) to select the service. Unmatched hosts: return 404 by default; optionally proxy uncached (`passthrough` mode). |
| FR-03 | P0 | Upstream is the original `Host`, resolved through a **dedicated resolver** (`UPSTREAM_DNS`, defaults to public resolvers) so the proxy never resolves through the intercepting DNS and loops. IPv4 and IPv6. |
| FR-04 | P0 | Upstream scheme per service: same-as-client (`http`) by default, `https` selectable; TLS verification with system roots. |
| FR-05 | P0 | `GET` is cached; `HEAD` is answered from metadata when known, otherwise proxied; all other methods proxied uncached. |
| FR-06 | P0 | Forward client request headers (notably `User-Agent`, service-specific headers); strip hop-by-hop headers; preserve upstream response headers on cached objects (`Content-Type`, `ETag`, `Last-Modified`, `Cache-Control` as received). |
| FR-07 | P0 | Add `X-Cache: HIT|MISS|PARTIAL|BYPASS` and `X-LanCache-Processed-By: <hostname>`; serve `GET /lancache-heartbeat` → `204` with `X-LanCache-Processed-By` and the CORS headers prefill tools and LANCache Manager expect. |
| FR-08 | P1 | Port 443 SNI pass-through: read the ClientHello, resolve the SNI host via the dedicated resolver, splice bytes both ways. No caching, no decryption. Replaces `sniproxy`. |
| FR-09 | P1 | Per-service upstream concurrency limit and global connection limits with backpressure. |

### 6.2 Slicing and range semantics

| ID | P | Requirement |
|---|---|---|
| FR-10 | P0 | Configurable slice size (default 1 MiB), overridable per service; persisted with the cache so a change is detected at startup and refused unless explicitly forced (lancache `CONFIGHASH` behaviour). |
| FR-11 | P0 | Serve full `GET` and any single byte range from slices with correct `200/206`, `Content-Length`, `Content-Range`, `Accept-Ranges: bytes`. Multi-range requests are answered with the full object (permitted by RFC 9110). |
| FR-12 | P0 | Fetch only the slices a request needs, as aligned `Range` sub-requests to upstream; stream to the client in order as slices arrive. |
| FR-13 | P0 | If upstream ignores `Range` (returns `200`), consume the full stream, slice it into the store on the fly, and still satisfy the client's requested range. Remember `no-ranges` per object. |
| FR-14 | P0 | Store validators and total length per object. A slice whose validators differ from the object's current generation invalidates the object (bump generation); an in-flight client stream is aborted so the client retries against the new version. |
| FR-15 | P0 | Correct `416` handling, zero-length objects, objects without `Content-Length` (cache only after complete). |
| FR-16 | P1 | Read-ahead: when a client streams sequentially, prefetch the next N slices (configurable window) to keep line rate on cold content. |
| FR-17 | P1 | Honour client `If-Range` (match → `206`, mismatch → full `200`). |
|FR-18  | P0 | Follow upstream `301/302/307` *server-side* (bounded hops, target host must resolve to a public address) and cache the content under the original key; never cache the redirect itself. Sony/PS5 CDNs redirect between hosts mid-object; monolithic solves this with an internal redirect-following upstream, and without it PS5 updates fail partway.
### 6.3 Cache policy

| ID | P | Requirement |
|---|---|---|
| FR-20 | P0 | Ignore upstream `Cache-Control`/`Expires` by default; cache `200` and `206` responses for `CACHE_MAX_AGE` (default `3560d`, matching monolithic). Per-service override to honour upstream headers. |
| FR-21 | P0 | Cache key = identifier + normalised path; query string dropped by default; per-service rules for keep-query, include-host, path rewrites, include/exclude regexes. Ship rules that reproduce monolithic's current per-service behaviour. |
| FR-22 | P0 | Never cache 3xx/4xx/5xx. On upstream `5xx`/timeout, serve any cached slices and fail only the missing ones (stale-on-error). |
| FR-23 | P2 | Optional revalidation (conditional `GET`) when an object's age exceeds a per-service threshold. |

### 6.4 Concurrency

| ID | P | Requirement |
|---|---|---|
| FR-30 | P0 | Single-flight per slice: concurrent requests for a missing slice share one upstream fetch; all waiters receive the slice when it lands. |
| FR-31 | P0 | A client disconnect does not cancel an in-flight slice fetch (configurable); the slice completes and is stored. |
| FR-32 | P1 | Object-level single-flight for the `no-ranges` path so one full-object stream feeds every waiting client. |

### 6.5 Storage

| ID | P | Requirement |
|---|---|---|
| FR-40 | P0 | Hybrid store: RAM tier (`CACHE_MEM_SIZE`) for hot slices and in-flight data; disk tier (`CACHE_DISK_SIZE`) as the capacity tier. Both are hard caps. |
| FR-41 | P0 | Eviction keeps the disk tier under its cap without blocking the serving path; recency/frequency-aware (S3-FIFO or LRU class), not pure FIFO. |
| FR-42 | P0 | Crash safety: a slice is either fully present or absent; a checksum per slice is verified on read (configurable); corrupt slices are dropped and refetched, never served. |
| FR-43 | P0 | Restart recovery: serve hits within seconds; full index rebuilt in the background; no dependency on a clean shutdown. |
| FR-44 | P0 | Slices are self-describing (carry object validators, length, generation) so the object index can be rebuilt from the data. |
| FR-45 | P0 | Works on a plain directory on ext4/XFS/ZFS/btrfs, HDD or SSD, as non-root. Raw block device support is a plus, not a requirement. |
| FR-46 | P1 | `MIN_FREE_DISK`-style guard: reduce the effective cap when the filesystem runs low. |
| FR-47 | P2 | Import tool for an existing monolithic nginx cache directory. |

### 6.6 Observability

| ID | P | Requirement |
|---|---|---|
| FR-50 | P0 | Prometheus `/metrics` on a separate admin port: requests, bytes served and bytes fetched by service and cache status; upstream latency histograms; in-flight fetches; store size, entries, evictions, checksum failures; connection counts. Bounded label cardinality (no per-URL labels). |
| FR-51 | P0 | Structured JSON logs to stdout with per-request events (client IP, service, host, path, range, status, bytes, cache status, upstream time). |
| FR-52 | P1 | Optional `lancache` access-log format (field-for-field with monolithic's `cachelog`) to a file or stdout so LANCache Manager / DeveLanCacheUI keep working. |
| FR-53 | P0 | `/healthz` (process up) and `/readyz` (store initialised, listeners bound). |
| FR-54 | P1 | Admin API: stats, list services, purge by service/path prefix, reload domain list, drain. Local/cluster-only by default; optional bearer token. |
| FR-55 | P2 | OpenTelemetry traces for request/slice/upstream spans. |

### 6.7 Configuration and lifecycle

| ID | P | Requirement |
|---|---|---|
| FR-60 | P0 | All settings via environment variables (see §8), reusing monolithic's names where the meaning matches; optional config file for per-service rules; precedence env > file > defaults. |
| FR-61 | P0 | `cache-domains` list bundled at build time; optional periodic refresh from the upstream repo with ETag caching; validate before applying; hot-reload via `SIGHUP` or admin API. |
| FR-62 | P0 | Graceful shutdown: stop accepting, finish in-flight slices, flush, exit within a bounded time. |
| FR-63 | P0 | Runs as non-root; binds privileged ports via `CAP_NET_BIND_SERVICE` or is fronted by a Service/port-map. |
| FR-64 | P0 | Refuses to proxy to private/loopback upstream addresses unless configured, and only proxies allow-listed hosts unless `passthrough` is on (the cache is an open proxy on the LAN otherwise). |

### 6.8 Distribution

| ID | P | Requirement |
|---|---|---|
| FR-70 | P0 | Multi-arch OCI image (`linux/amd64`, `linux/arm64`), minimal base, signed, with SBOM. |
| FR-71 | P0 | Helm chart: single-replica workload with `Recreate` strategy, PVC, `LoadBalancer`/`hostNetwork` service on 80/443 with `externalTrafficPolicy: Local`, ServiceMonitor, Grafana dashboard ConfigMap, resource/affinity knobs, `helm test`. Published as OCI. |
| FR-72 | P0 | `docker compose` example mirroring the lancache quickstart (DNS + cache). |
| FR-73 | P1 | Static binaries for Linux (amd64/arm64) and macOS; Windows best-effort. |
| FR-74 | P0 | Documentation site with quickstart, configuration reference, migration-from-lancache guide, Kubernetes guide, benchmark methodology. |

## 7. Non-functional requirements

| ID | Requirement | Target |
|---|---|---|
| NFR-1 | Hit throughput | ≥ 1.1 GB/s aggregate from NVMe with ≥ 8 concurrent clients on a 10 GbE host; on HDD arrays, within 5 % of monolithic on the same volume |
| NFR-2 | Hit latency | p99 time-to-first-byte < 5 ms for RAM-tier hits, < 25 ms for disk-tier hits (SSD) |
| NFR-3 | Miss overhead | Fill throughput ≥ 95 % of direct download; extra TTFB ≤ 10 ms |
| NFR-4 | Concurrency | 10 000 open client connections; 500 in-flight upstream fetches |
| NFR-5 | Memory | RSS ≤ configured RAM tier + index + 512 MiB baseline; no per-connection buffers larger than a slice |
| NFR-6 | Startup | Serving within 5 s; index for a 2 TB cache fully rebuilt in < 5 min |
| NFR-7 | Integrity | Zero corrupt bytes served in a 7-day soak with checksum verification on |
| NFR-8 | Footprint | Single static binary; image < 40 MB compressed |
| NFR-9 | Portability | Linux amd64/arm64 tier 1; macOS builds and tests pass; Windows not blocked by design |
| NFR-10 | Security | No TLS termination; non-root; allow-listed upstreams by default; no secrets required |

## 8. Configuration surface (env)

| Variable | Default | lancache equivalent | Notes |
|---|---|---|---|
| `CACHE_DISK_SIZE` | `1000g` | same | Disk tier cap; nginx-style units |
| `CACHE_MEM_SIZE` | `2g` | same (different meaning) | RAM tier for hot slices; index memory is reported separately |
| `CACHE_MAX_AGE` | `3560d` | same | Object TTL |
| `CACHE_SLICE_SIZE` | `1m` | same | Persisted; change requires `FORCE_CONFIG=true` |
| `UPSTREAM_DNS` | `1.1.1.1 1.0.0.1` | same | Dedicated resolver |
| `MIN_FREE_DISK` | `10g` | same | Effective cap guard |
| `CACHE_DOMAINS_REPO` / `CACHE_DOMAINS_REFRESH` | upstream repo / `24h` | `CACHE_DOMAINS_*` | Refresh of the hostname list |
| `HTTP_PORT` / `HTTPS_PORT` / `ADMIN_PORT` | `80` / `443` / `9090` | — | |
| `PASSTHROUGH_UNKNOWN_HOSTS` | `false` | — | |
| `LOG_FORMAT` | `json` | — | `json` or `lancache` |
| `LOG_LEVEL` | `info` | — | |
| `READAHEAD_SLICES` | `4` | — | |
| `UPSTREAM_MAX_INFLIGHT` | `256` | — | Global; per-service via config file |
| `CACHE_DATA_DIR` | `/data/cache` | volume path | |

## 9. Compatibility and ecosystem

- **lancache-dns / cache-domains**: consumed as-is; the same hostname list drives service matching.
- **Prefill tools**: detect the cache through DNS plus the `/lancache-heartbeat` header; nothing else is required.
- **LANCache Manager**: log-based features work with `LOG_FORMAT=lancache`; cache-directory features (browse/purge/corruption scan) will not, because the store is different. Plan: publish an admin API and offer an upstream contribution to LANCache Manager so it can use either backend.
- **Steam**: the client discovers a LAN cache by resolving `lancache.steamcontent.com` and then downloads over plain HTTP through it, so Steam's own HTTPS use elsewhere does not affect caching.
- **PlayStation (PS4/PS5)**: `sony` group in cache-domains. Consoles fetch `.pkg` files over plain HTTP in ~18 MB ranges with a per-console query string (`serverIpAddr=`), so the key must drop the query; upstreams redirect between Sony hosts mid-object (FR-18); consoles hash-verify packages and fail hard on corrupt bytes (FR-14, FR-42). Store, auth and licensing are HTTPS pass-through. No prefill tool exists.
- **Xbox**: `xboxlive` group plus `windowsupdates` (console and Game Pass content also rides Windows Update hosts). Licensing over HTTPS, content over HTTP. The regix1 Xbox prefill daemon works through any lancache-compatible proxy. Community reports of Xbox bypassing the cache exist; treat as best-effort until verified with real traffic.
- **Consoles in general**: they must resolve through the intercepting DNS (DHCP-assigned or set per console); a console with hard-coded public DNS bypasses the cache silently.
- 
## 10. Success metrics and acceptance criteria

1. **Parity benchmark** (see plan §10): on identical hardware and volume, hit throughput, CPU per Gbps, p99 TTFB and hit ratio over a replayed trace are ≥ monolithic; results published in the docs.
2. **Correctness**: differential test suite shows byte-identical output to upstream for full GETs and randomised ranges, across range-capable, range-ignoring, flaky and validator-changing upstreams.
3. **Real-world**: SteamPrefill, Epic and Battle.net prefill runs complete against real CDNs through `lancache-dns` with no cache errors, then replay at LAN speed.
4. **Operations**: `helm install` on the Talos cluster with ≤ 10 values; metrics visible in Grafana; pod restart resumes serving hits within 5 s.
5. **Soak**: 7 days under mixed load with zero checksum failures and RSS within limits.

## 11. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| CDNs move content to HTTPS | Uncacheable for everyone, not just us | Steam has explicit LAN-cache support; SNI pass-through keeps clients working; track `cache-domains` |
| CDN quirks (ignored ranges, unstable ETags, redirects, chunked bodies) | Correctness / hit ratio | Per-service rules; `no-ranges` path; generation invalidation; chaos tests with a mock CDN |
| Cache-library API churn (foyer is pre-1.0; Pingora's cache API is labelled experimental) | Rework | Pin versions; isolate behind a store trait; spike before committing (plan M0) |
| Ecosystem tools bound to nginx internals | Adoption | `lancache` log format; admin API; upstream PRs to LANCache Manager |
| Eviction thrash on small caches with huge objects (5 GB ESDs) | Hit ratio | Slice-level eviction with object-aware scoring; measure in benchmark |
| Disk full / IO errors | Outage | Degrade to pass-through, alert via metrics, `MIN_FREE_DISK` |
| Single maintainer bandwidth | Delivery | MVP scope discipline (P0 only), CI that enforces quality, docs from day one |

## 12. Open questions

1. Should the RAM tier default to a fraction of the container limit instead of a fixed size?
2. Is nginx cache import (FR-47) worth the effort, or is a re-prefill acceptable for migrators?
3. Do we adopt lancache's exact env names or a prefixed set (`CACHIC_*`) with aliases?

## 13. Release plan (summary)

| Version | Contents |
|---|---|
| 0.1 (MVP) | P0 proxying, slicing, store, metrics, JSON logs, env config, image, compose example |
| 0.2 | Robustness: no-ranges path, generations, stale-on-error, admin API, crash-recovery tests |
| 0.3 | Helm chart, Kubernetes docs, benchmark report vs monolithic, `lancache` log format |
| 1.0 | SNI pass-through, read-ahead, per-service rule parity, domain refresh, Grafana dashboard, 7-day soak passed |
| 1.x | Revalidation, nginx cache import, OpenTelemetry, block-device store, sharding exploration |

## 14. References

- lancache.net — Monolithic container, FAQ, tuning and config-hash docs: https://lancache.net/docs/
- uklans/cache-domains: https://github.com/uklans/cache-domains
- Steam LAN-cache discovery (`lancache.steamcontent.com`): https://www.ctrl.blog/entry/steam-lancache-protocol.html
- LANCache Manager: https://github.com/regix1/lancache-manager
- foyer (hybrid cache, Rust): https://github.com/foyer-rs/foyer
- Pingora (proxy framework, Rust): https://github.com/cloudflare/pingora