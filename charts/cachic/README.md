# cachic Helm chart

```sh
helm install cachic oci://ghcr.io/leftathome/charts/cachic \
  --set service.loadBalancerIP=192.168.1.10 \
  --set persistence.storageClass=local-path \
  --set persistence.size=2200Gi
```

Then point `lancache-dns` (or Pi-hole, AdGuard, Unbound) at `service.loadBalancerIP`, and clients
use the cache with no client-side configuration.

## Storage: read this before choosing a class

**A replicated volume is the wrong shape for a cache.** Longhorn's default of three replicas means
every byte written to the cache is written three times across the network, and the read path is
capped by the replication layer rather than by the disk. You are paying availability cost for data
that is, by definition, re-downloadable.

In order of preference:

1. **A local PV or `hostPath` on a node with fast disks**, pinned with `nodeSelector`. This is what
   the cache actually wants: direct access to an NVMe device. The cache is not highly available
   and does not need to be - if the node is down, clients fetch from the internet, which is what
   they would do without a cache at all.
2. **Longhorn with `numberOfReplicas: "1"`** and strict-local data locality, if you want the
   volume managed but not replicated.
3. **NFS or iSCSI from a NAS over 10 GbE**, which trades latency for capacity. Workable, and the
   right answer if your bulk storage lives there anyway.

`persistence.size` must exceed `cache.diskSize`: the object index and filesystem overhead share
the volume. The chart's default leaves 100Gi of headroom on a 1000g cache.

## Sizing

`cache.memSize` is the RAM tier and does **not** include the object index. Budget roughly 400
bytes per stored slice on top of it - about 760Mi for a 2TB cache at 1m slices, or 190Mi at 4m
slices. `resources.requests.memory` should cover `cache.memSize` plus the index plus a baseline.

Per-connection memory is `cache.readaheadSlices * cache.sliceSize`, which is 4Mi at the defaults.
That is a hard bound, not a target.

Changing `cache.sliceSize` against an existing volume **refuses to start**: the stored slices were
written under the old size and cannot be reinterpreted. Set `FORCE_CONFIG=true` to adopt the new
size and abandon the existing cache.

## Why one replica

The deployment is fixed at one replica with a `Recreate` strategy. Two replicas cannot share a
ReadWriteOnce volume, and two replicas with separate volumes halve the hit rate while doubling
upstream traffic - the opposite of the point. A rolling update would require the volume attached
to two pods at once, so `Recreate` is not a limitation to work around but the correct strategy.

## DNS

`upstreamDns` must **not** be the lancache DNS server. That server answers CDN hostnames with this
cache's address; resolving through it would loop every upstream fetch back into the cache.

This is safe by construction rather than by configuration: cachic resolves upstreams through
`upstreamDns` and never the system resolver, and the constructors that read `/etc/resolv.conf` are
not compiled into the binary. `dnsPolicy` and `dnsConfig` therefore affect only cluster-internal
lookups, not upstream fetches.

## Admin API

The admin port is a separate ClusterIP service and is deliberately not exposed through the load
balancer - it carries purge and drain. Set `admin.token`, or point `admin.existingSecret` at a
Secret you manage (External Secrets, sealed-secrets, whatever your cluster uses), if anything on
the cluster network should not have it.

## Verifying an install

`helm test` probes the heartbeat endpoint and readiness:

```sh
helm test cachic
```

A 204 with `X-LanCache-Processed-By` means prefill tools and LANCache Manager will detect the
cache. Beyond that, watch `foyer_storage_inner_op_total{op="channel_overflow"}` on `/metrics`: if
it is climbing, writes are outrunning the disk and the cache is silently declining to store
content.
