# Deploying with Flux

```sh
kubectl apply -k deploy/flux/
```

Four objects, one per file: the namespace, the chart source, the release, and the kustomization
tying them together.

Two values are the ones you actually change: `service.loadBalancerIP`, which is the address your
DNS server answers CDN hostnames with, and the storage settings.

## Why the chart version is pinned to a range

`semver: ">=0.1.0 <0.2.0"` rather than a floating tag. An unattended cache that upgrades itself
across a major version at 3am is not a feature. Patch and minor releases are safe to take
automatically; a major one should be a decision.

## Why automatic rollback is off

`upgrade.remediation.retries: 0`. A failed upgrade that rolls back can leave a data volume written
by a newer store format and then read by an older binary. The config guard would refuse to start,
which is the correct behaviour and a confusing thing to debug at the same time as whatever caused
the upgrade to fail. Failing visibly is better.

## Storage and node pinning

The example pins the pod to a specific node with `nodeSelector`, because the volume is a local PV
on that node's NVMe. This is deliberate: see the [chart README](../../charts/cachic/README.md) for
why a replicated volume is the wrong shape for a cache.

If the node is down, clients fetch from the internet. That is the same thing they would do without
a cache, so the loss is throughput, not availability.

## The load balancer

`externalTrafficPolicy: Local` is the chart default and matters here. With `Cluster`, traffic can
hop to another node and arrives with the wrong source address, so the access log and any
per-client analysis see the node instead of the client. `Local` also avoids the extra hop.

With MetalLB or Cilium LB-IPAM, put the pool annotation in `service.annotations`.

## Cluster DNS

`dnsPolicy` and `dnsConfig` affect only cluster-internal lookups. cachic resolves upstream CDN
hostnames through `upstreamDns` and never the system resolver - the constructors that read
`/etc/resolv.conf` are not compiled into the binary - so cluster DNS cannot reintroduce the
resolution loop even if it is pointed at the lancache DNS server.

## Upgrades

The PVC carries `helm.sh/resource-policy: keep`, so uninstalling the release does not delete the
cache. Refilling 2 TB takes days; that is not something to lose to a `helm uninstall`.

An upgrade reuses the volume. The only change that will not: `cache.sliceSize`, which the config
guard refuses because the stored slices cannot be reinterpreted under a new size.
