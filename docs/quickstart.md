# Quickstart with docker compose

```sh
cd deploy/compose
export LANCACHE_IP=192.168.1.10        # this host's LAN address
docker compose up -d
```

Then point your clients' DNS at `LANCACHE_IP`. Nothing on the client changes.

## What the two containers do

`dns` answers CDN hostnames with `LANCACHE_IP` so clients reach the cache without any client-side
configuration. Everything else it forwards upstream. This is `lancachenet/lancache-dns`, unchanged
— cachic deliberately does not do DNS interception.

`cache` is cachic. It resolves upstream hostnames through `UPSTREAM_DNS`, never the system
resolver, so it cannot be tricked into resolving a CDN hostname through the DNS server that points
at itself. That is a structural guarantee rather than a configuration you can get wrong: the
constructors that read `/etc/resolv.conf` are not compiled into the binary.

## Sizing

- `CACHE_DISK_SIZE` is a hard cap on the data volume.
- `CACHE_MEM_SIZE` is the RAM tier, and does **not** include the object index. Budget roughly
  400 bytes per stored slice on top: about 760 MB for a 2 TB cache at 1 MiB slices.
- Changing `CACHE_SLICE_SIZE` against an existing volume refuses to start. The stored slices were
  written under the old size and cannot be reinterpreted. Set `FORCE_CONFIG=true` to adopt the new
  size and abandon the existing cache.

See the [configuration reference](./configuration.md) for every setting.

## Checking it works

```sh
curl -i -H 'Host: lancache.steamcontent.com' http://LANCACHE_IP/lancache-heartbeat
```

A `204` with an `X-LanCache-Processed-By` header means prefill tools and LANCache Manager will
detect the cache.

Metrics are on `127.0.0.1:9090/metrics`. Watch
`foyer_storage_inner_op_total{op="channel_overflow"}`: if it is climbing, writes are outrunning
the disk and the cache is silently declining to store content.
