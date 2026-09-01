# cachic - Project Architecture

**Purpose**: Component map, request flow and storage model for the cachic proxy.
**Authoritative source**: `docs/cachic-IMPLEMENTATION-PLAN.md` sections 1-3. This doc is the
working summary; update it as code lands and the plan drifts.
**Updated**: 2026-09-01
**Status**: Design only - no code exists yet.

---

## Overview

cachic is one process that terminates plain HTTP on :80, serves game/OS CDN content out of a
hybrid RAM+disk cache, and passes TLS through on :443. Clients reach it because DNS (provided by
`lancache-dns` or similar - not by cachic) answers CDN hostnames with the cache's IP.

**Why it exists**: `lancachenet/monolithic` does this with nginx plus generated config. Expressing
it as an application buys slice-aware caching in cache terms rather than nginx terms, real
observability (`/metrics`, structured logs), request coalescing that streams the in-flight fill
instead of blocking on a lock, an admin API, and a Kubernetes-native deployment.

**Explicit non-goals (v1)**: DNS interception, TLS interception, general RFC 9111 semantics,
clustering, on-disk compatibility with nginx's cache dir, a web UI, Windows as a host platform.

---

## Components

```
+------------------------------------------------------------------------+
| cachic (one process)                                                   |
|                                                                        |
|  listeners --+-- :80   HTTP/1.x server (hyper) --> router --> orchestr. |
|              +-- :443  SNI pass-through (tokio TCP + ClientHello peek)  |
|              \-- :9090 admin (metrics, health, purge, reload)          |
|                                                                        |
|  services    : cache-domains loader, service rules, key normaliser     |
|  orchestrator: range parse -> slice plan -> fetch/serve pipeline       |
|  upstream    : HTTP client pool, dedicated resolver, limits, retries   |
|  store       : foyer HybridCache<SliceKey, SliceValue> + object index  |
|  telemetry   : tracing (json), metrics, access log formats             |
+------------------------------------------------------------------------+
```

Module layout inside the binary crate (`crates/cachic/src/`): `config/`, `services/`, `proxy/`,
`orchestrator/`, `upstream/`, `store/`, `sni/`, `admin/`, `telemetry/`, wired in `main.rs`.
A dev-only `crates/cachic-testkit/` holds `mockcdn`, the load generator and trace replay.

---

## Request Flow

1. Parse request; take `Host` (port stripped) and match against compiled service matcher
   (exact + wildcard). No match -> 404 or pass-through.
2. Non-`GET`/`HEAD` -> pass-through. `HEAD` -> answer from the object index if known, else proxy.
3. Normalise the cache key per service rule -> `object_id = blake3(identifier || normalised_key)[..16]`.
4. Parse `Range`; single range only, multi-range treated as full.
5. Look up object metadata (length, validators, generation, `no_ranges`). Unknown -> **probe**:
   fetch the first needed slice with `Range: bytes=a-b`. A `206` yields total length and validators
   from `Content-Range`/`ETag`/`Last-Modified`; a `200` marks the object `no_ranges` and switches
   to the full-stream path.
6. Compute slice indices `[i0..=i1]`; send response headers (`200`/`206`, `Content-Length`,
   `Content-Range`, validators, `Accept-Ranges`, `X-Cache`, `X-LanCache-Processed-By`).
7. Pipeline: for each `i`, `store.fetch((object_id, gen, i), || upstream.fetch_slice(..))` with a
   bounded `READAHEAD_SLICES` window. foyer's `fetch` coalesces concurrent misses per key. Write
   each slice's relevant byte range to the body **in order**.
8. Validator mismatch in any slice -> bump generation, abort the client stream (connection close
   makes the client retry), log and count.
9. Emit the access event with cache status: `HIT` (all slices from store), `MISS` (none),
   `PARTIAL` (mixed).

The `no_ranges` path uses an object-level filler registry (`DashMap<object_id, Arc<FillState>>`):
one task streams the full body, cuts slices into the store and publishes per-slice readiness;
other requests subscribe rather than issue their own fetch.

---

## Storage Model

- **Slice key**: `(object_id: [u8;16], generation: u32, index: u32)`.
- **Slice value**: header (`magic`, `slice_size`, `total_len`, `etag`, `last_modified`,
  `content_type`, `generation`, `xxh3` of payload) + payload. Slices are **self-describing**, so
  the object index is a rebuildable acceleration structure, never the source of truth.
- **Store**: foyer `HybridCache`. Memory tier sized by `CACHE_MEM_SIZE`; disk tier on
  `CACHE_DATA_DIR` sized by `CACHE_DISK_SIZE`. Eviction, admission, recovery, dedup and metrics
  come from foyer. Direct IO vs page cache is a tunable to measure in M0.
- **Object index**: `redb` table `object_id -> {key, len, validators, gen, no_ranges, created,
  last_seen}`. `last_seen` updated at most hourly per object; entries pruned by `CACHE_MAX_AGE`.
  A missing entry means a cheap probe; if a slice is found, the index is repaired from its header.
- **Config guard**: `slice_size` and store format version are written to `CACHE_DATA_DIR/CONFIG`.
  A mismatch aborts startup unless `FORCE_CONFIG=true`.

---

## Non-Obvious Behaviours

These are the things that will be "fixed" incorrectly by someone who has not read this list:

- **Own resolver, always.** Upstreams are resolved with `hickory-resolver` using `UPSTREAM_DNS`,
  never the system resolver. In Kubernetes the pod resolver may forward to the very DNS server
  that is intercepting CDN names - using it creates a loop back into the cache.
- **Private-address guard.** Upstream targets resolving to private/loopback ranges are refused
  unless explicitly configured (FR-64).
- **Client disconnect does not cancel a fill** (FR-31). Fills are tasks owned by the store, not by
  the connection.
- **Backpressure is the read-ahead window.** Slice futures are awaited in order; RAM per
  connection is bounded by `READAHEAD_SLICES * slice_size`.
- **Immutable-object semantics, not RFC 9111.** TTLs are operator-defined; upstream freshness
  headers do not drive expiry.

---

## Repository Layout (planned)

```
cachic/
|-- Cargo.toml                 # workspace
|-- rust-toolchain.toml        # pinned stable
|-- deny.toml                  # cargo-deny (licences, advisories, bans)
|-- justfile                   # dev loop: fmt, lint, test, bench, image, chart
|-- crates/
|   |-- cachic/                # the binary (modules listed above)
|   \-- cachic-testkit/        # mockcdn, load generator, trace replay (dev-dep)
|-- charts/cachic/             # Helm chart
|-- deploy/compose/            # docker compose examples (dns + cache)
|-- deploy/flux/               # HelmRelease + OCIRepository example
|-- docs/                      # mdBook: quickstart, config, k8s, migration, benchmarks, adr/
|-- dashboards/                # Grafana JSON
\-- Dockerfile
```

Today the repo contains only `docs/` (PRD + plan) and `.agent/`. The skeleton above is an M0
deliverable.

---

## Milestones

| Milestone | Scope | Exit criteria | Status |
|---|---|---|---|
| M0 | Spike (hyper + reqwest + foyer), hardware measurements, ADRs, repo skeleton | >= 8 Gbps hit throughput on the NUC with 8 clients; foyer per-entry memory known; Rust go/no-go confirmed | Not started |
| M1 | MVP proxy v0.1 | SteamPrefill and Epic prefill complete through the proxy; differential tests pass; non-root image on amd64 + arm64 | Not started |
| M2 | Robustness v0.2 | Chaos suite green; 48-hour homelab soak | Not started |
| M3 | Deployment and parity v0.3 | Chart installed on Talos via Flux with <= 10 values; benchmark parity on every scenario | Not started |
| M4 | v1.0 | 7-day soak; docs complete; per-service rule parity reviewed | Not started |

---

## Open Decisions

- Pingora vs hyper for the server (default hyper; resolved in M0).
- Rust vs Go - Appendix A of the plan keeps the Go path costed at ~1 day to switch after M0.
- Canonical hosting (GitHub vs GitLab) - CI is written so either can be the mirror.
- ADRs to write in M0: language, store, index, slice size and key scheme, config surface, hosting.

---

## Related Documentation

- [Tech Stack Patterns](./tech-stack-patterns.md)
- `docs/cachic-PRD.md`, `docs/cachic-IMPLEMENTATION-PLAN.md`

## Change Log

### 2026-09-01 - Initial creation
- Summarised from PRD v0.1 and implementation plan v0.1 at Navigator init. No code yet.
