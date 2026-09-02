# Running cachic on Kubernetes

## The shape of the deployment

One pod, one volume, a LoadBalancer service on 80 and 443, and a separate ClusterIP service for
the admin port. There is no horizontal scaling story and that is deliberate: two replicas cannot
share a ReadWriteOnce volume, and two replicas with separate volumes halve the hit rate while
doubling upstream traffic.

## Getting traffic to it

Clients reach the cache because DNS says so. Run `lancache-dns`, or configure Pi-hole, AdGuard or
Unbound to answer the `cache-domains` hostnames with the cache's address, and set that as your
DHCP-assigned resolver.

Consoles are the common failure: a console with hard-coded public DNS bypasses the cache silently,
with no error anywhere. If a PlayStation or Xbox appears not to be caching, check its DNS settings
before anything else.

## Storage

See the [chart README](../charts/cachic/README.md#storage-read-this-before-choosing-a-class). The
short version: a local PV on fast disks, pinned with `nodeSelector`. A replicated volume writes
every byte three times for data that is by definition re-downloadable.

## Probes

`/healthz` is liveness and deliberately does not consult the store: a probe that restarts the
process because a disk failed prevents you from reading why. `/readyz` is readiness and does
consult it, so a pod that is opening a large cache directory reports not-ready rather than taking
traffic.

The admin port binds before the store opens, so both probes answer throughout startup rather than
refusing connections. Give liveness a generous `failureThreshold`: recovery on a large cache takes
time, and restarting mid-recovery is exactly wrong.

## Shutdown

The process drains within 20 seconds: readiness fails immediately so the load balancer moves
traffic away, in-flight requests finish, and it exits. `terminationGracePeriodSeconds: 30` leaves
headroom. Fills already in progress are allowed to complete - abandoning one wastes the bytes
already fetched.

## What to watch

`/metrics` on the admin port. Beyond the obvious hit-ratio and throughput series, two counters
matter more than they look:

- `foyer_storage_inner_op_total{op="channel_overflow"}` - disk writes discarded because the
  flushers fell behind. **Non-zero means the cache is silently declining to cache.** This is the
  product's worst failure mode and is invisible without this counter.
- `cachic_checksum_failures_total` - slices that failed verification on read. Should be zero; a
  non-zero value means the storage underneath is returning bad bytes.

## Sizing

| | |
|---|---|
| `cache.diskSize` | Hard cap on cached content |
| `cache.memSize` | RAM tier. Does **not** include the object index |
| Object index | ~400 bytes per stored slice: ~760Mi for 2TB at 1m slices |
| Per connection | `readaheadSlices * sliceSize`, 4Mi at defaults |

`resources.requests.memory` should cover `memSize` + index + a baseline. The chart's defaults
assume a 1000g cache; scale the request with `diskSize`, not just with `memSize`.

## Upgrades

Reuse the volume. The one setting that cannot change in place is `cache.sliceSize`: the stored
slices were written under the old size and cannot be reinterpreted, so startup refuses. Set
`FORCE_CONFIG=true` to adopt the new size and abandon the existing cache.
