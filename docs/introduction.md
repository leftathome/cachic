# cachic

An HTTP caching proxy for game-distribution and OS-update CDN traffic. It replaces the nginx
engine inside `lancachenet/monolithic` while keeping lancache's deployment model: a DNS server
answers CDN hostnames with the cache's IP, clients speak plain HTTP to the cache, and the cache
fetches, slices, stores and serves content that is effectively immutable.

## Why it exists

`lancachenet/monolithic` works. What it is, though, is nginx plus generated configuration, and
that shows in day-to-day operation:

- Tuning is expressed in nginx terms — `keys_zone` sizing, slice directives, loader parameters —
  rather than in cache terms.
- Observability is an access log on a volume. Every dashboard in the ecosystem is a log tailer,
  because there was nothing else to consume.
- Coalescing relies on `proxy_cache_lock`, where waiters block on a lock rather than streaming the
  fill already in progress.
- There is no admin API and nowhere to put behaviour like read-ahead or integrity checks.

Expressed as an application, those are small and testable.

## What it does differently

**Coalescing streams rather than blocks.** Thirty clients starting the same download produce one
upstream fetch per slice, and all of them stream the fill in progress. Measured upstream
amplification on a cold object is exactly 1.00.

**Fills outlive their connection.** A client that disconnects mid-download does not abandon the
slice it was fetching; the next client gets work already paid for.

**Validator changes invalidate rather than mix.** If an object is replaced upstream mid-download,
the response aborts and the client retries. No response ever contains two versions.

**Observability is first-class.** Prometheus metrics, structured logs, health probes and an admin
API, with the lancache access-log format available as a compatibility shim for existing
dashboards.

## What it deliberately does not do

- **DNS interception.** `lancache-dns`, Pi-hole, AdGuard and Unbound already do this well.
- **TLS interception.** Port 443 is SNI pass-through only. Caching HTTPS would mean installing a
  MITM certificate on every client.
- **General RFC 9111 semantics.** This is an immutable-object cache with operator-defined TTLs.
- **Clustering.** One node covers the target users.

## Status

Pre-alpha. The proxy works end to end, but there are no published benchmarks against monolithic,
no tagged release, and it has not run against real game clients at scale.

## Two things to know before deploying it

**Watch `foyer_storage_inner_op_total{op="channel_overflow"}`.** If it is climbing, writes are
outrunning the disk and the cache is silently declining to store content while still serving
clients at full speed. It is this product's worst failure mode and invisible without that counter.

**`UPSTREAM_DNS` must not be your lancache DNS server.** That server answers CDN hostnames with
the cache's own address, so resolving through it would loop every fetch back into the cache. This
is safe by construction: the constructors that read `/etc/resolv.conf` are not compiled into the
binary.
