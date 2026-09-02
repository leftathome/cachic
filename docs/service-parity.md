# Per-service parity with monolithic

G1 is functional parity for every service in `uklans/cache-domains`. This is the review of that
claim, service by service, against `lancachenet/monolithic`'s actual nginx configuration rather
than against assumptions about it.

## How monolithic builds a cache key

```nginx
proxy_cache_key $cacheidentifier$uri$slice_range;
```

Three things follow, and cachic matches all three:

- **The service identifier, not the host.** This is the whole point: `cdn1.epicgames.com/x` and
  `cdn2.epicgames.com/x` are the same bytes and share one cached object.
- **`$uri`, not `$request_uri`.** nginx's `$uri` excludes the query string *and* is decoded and
  normalised. cachic normalises the path the same way — percent-decoding, collapsing duplicate
  slashes, resolving `.` and `..`. Keying on the raw path would cache the same object twice
  whenever a client sent `%41` for `A`.
- **The slice range.** cachic's slice key carries the index, which is the same idea.

## Services with special rules

Four, all exclusions, all transcribed from monolithic's `cache.conf.d/` with the source file named
in `services/defaults.rs`. Each is content that is small, changes often, and whose staleness
breaks a client.

| Service | Excluded | monolithic source |
|---|---|---|
| `riot` | `releaselisting_*`, `*.version` | `20_lol.conf` |
| `arenanet` | `/latest64` prefix | `21_arenanet_manifest.conf` |
| `wsus` | `authrootstl.cab`, `pinrulesstl.cab`, `disallowedcertstl.cab` (case-insensitive) | `22_wsus_cabs.conf` |
| `steam` | `/server-status` exactly | `23_steam_server_status.conf` |

`wsus` is worth calling out: those are certificate trust and revocation lists. A stale revocation
list is a security problem, not a cache miss.

The other 22 services in `cache-domains` need nothing beyond the defaults, which is itself the
finding — most of the list is "cache everything under this hostname".

## Deliberate divergences

**cachic preserves `ETag`; monolithic strips it.**

```nginx
proxy_hide_header ETag;
```

monolithic removes the header from responses. cachic keeps it and uses it: a validator change is
how an object is detected as replaced upstream, which triggers a generation bump so no response
mixes two versions (FR-14). Without validators there is no way to notice, and the alternative is
serving a mixture.

The visible effect is that a client may see an `ETag` from cachic where monolithic gave none. No
known client depends on its absence.

**cachic answers `PARTIAL`.** `X-Cache` may report `PARTIAL` for a range spanning cached and
uncached slices. monolithic reports `HIT` or `MISS` only. Dashboards treating anything that is not
`HIT` as a miss will read this correctly; one matching `MISS` exactly will undercount.

## What this review does not cover

Real client traffic. The rules above are transcribed correctly and unit-tested against the paths
monolithic's `location` blocks match, but no capture of actual Steam, Windows Update or Blizzard
requests has been replayed through both engines. That is the remaining part of TASK-31 and needs
either traffic captures or the reference hardware.

If a service turns out to need a rule we do not ship, it is a configuration change rather than a
release: add it to the rules file, and send it upstream to `cache-domains` if it belongs there.
