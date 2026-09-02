# Migrating from lancachenet/monolithic

## What carries over unchanged

**Your DNS setup.** cachic consumes `lancache-dns` and `uklans/cache-domains` as-is. The same
hostname list drives service matching, and clients need no change.

**Most of your environment.** Where monolithic's variable means the same thing, cachic uses the
same name and the same units. `CACHE_DISK_SIZE=1000g` means what it always meant — nginx-style
suffixes are binary multiples here too, so an existing value is not silently reinterpreted as 7%
smaller.

| Variable | Same meaning? |
|---|---|
| `CACHE_DISK_SIZE` | Yes |
| `CACHE_MAX_AGE` | Yes |
| `CACHE_SLICE_SIZE` | Yes |
| `UPSTREAM_DNS` | Yes, space-separated as before |
| `MIN_FREE_DISK` | Yes |
| `CACHE_MEM_SIZE` | **No — see below** |

**Your dashboards.** Set `LOG_FORMAT=lancache` and cachic emits monolithic's `cachelog` format,
field for field, so LANCache Manager, DeveLanCacheUI and lancache-ui keep working.

**Prefill tools.** SteamPrefill, Epic, Battle.net and Riot prefill detect the cache through DNS
plus the `/lancache-heartbeat` endpoint, which cachic answers identically.

## What does not carry over

**The cache directory.** cachic's store is a different format — self-describing slices rather than
nginx's hashed directory tree. There is no import, and none is planned for 1.0; the first fill
after migrating is a cold one. On a domestic connection that is measured in days for a large
cache, so migrate before an event, not during one.

**`CACHE_MEM_SIZE` means something different.** Under nginx it sized the shared-memory *index*.
Under cachic it sizes the RAM tier that holds hot slice *data*, and the index is separate and
reported on its own. Do not carry your old value across: budget roughly 400 bytes per stored slice
for the index on top of whatever you set here. A 2 TB cache at 1 MiB slices needs about 760 MB of
index, which is roughly three times what the equivalent nginx configuration used.

**LANCache Manager's cache-directory features.** Browsing and purging by walking nginx's hashed
directory cannot work against a different store. Its log-based views are unaffected. cachic offers
an admin API for the same operations (`/stats`, `/services`, `/purge?prefix=…`), and the intent is
to contribute backend support upstream so one tool can drive either engine.

**`CACHE_INDEX_SIZE` and the nginx tuning knobs.** `keys_zone` sizing, `proxy_cache_lock`, loader
parameters and slice directives have no equivalent, because there is no nginx. Everything is
expressed in cache terms instead: bytes on disk, bytes in RAM, slice size, read-ahead window.

## Migration steps

1. Stop monolithic.
2. Point cachic at a **new, empty** data directory. Do not reuse the nginx cache directory; cachic
   will not read it and will treat it as an unfamiliar volume.
3. Carry across `CACHE_DISK_SIZE`, `CACHE_MAX_AGE`, `CACHE_SLICE_SIZE`, `UPSTREAM_DNS` and
   `MIN_FREE_DISK` unchanged.
4. Set `CACHE_MEM_SIZE` deliberately rather than copying it, and size the container's memory limit
   to cover it plus the index.
5. Set `LOG_FORMAT=lancache` if you want your existing dashboards to keep working.
6. Verify with `curl -i -H 'Host: lancache.steamcontent.com' http://CACHE_IP/lancache-heartbeat`
   — a `204` with `X-LanCache-Processed-By` means the ecosystem will detect it.
7. Refill, ideally overnight, with a prefill tool.

## Behaviour that differs deliberately

**Coalescing streams rather than blocks.** Under nginx's `proxy_cache_lock`, clients waiting for a
slice being fetched by another client block on a lock. cachic's waiters stream the fill in
progress. The visible effect is that the second and subsequent clients on a cold object start
receiving bytes immediately rather than after the first client finishes.

**A client disconnect does not cancel a fill.** The slice completes and is stored, so the next
client benefits from work already paid for.

**Validator changes invalidate.** If an object changes upstream mid-download, cachic aborts the
response rather than splicing two versions together. The client retries and gets the new version.

## If something is not being cached

Check, in this order:

1. **DNS.** A client with hard-coded public DNS bypasses the cache silently. Consoles are the
   usual culprit.
2. **`foyer_storage_inner_op_total{op="channel_overflow"}` on `/metrics`.** If it is climbing,
   writes are outrunning the disk and the cache is declining to store content while still serving
   clients at full speed.
3. **The service list.** `GET /services` on the admin port shows what is matched.
