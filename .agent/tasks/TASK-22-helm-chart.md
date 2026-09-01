# TASK-22: Helm chart

## Context
Milestone: M3 | Requirements: FR-71, G6; plan section 7.2

Kubernetes-native delivery is a headline goal. The exit criterion is a Flux install on the Talos
cluster with ten values or fewer.

## Implementation Plan
### Phase 1: Workload
- [ ] Single replica, `Recreate` strategy (RWO volume)
- [ ] PVC with `storageClass`/`existingClaim`/`size`
- [ ] securityContext: non-root, read-only root filesystem, `NET_BIND_SERVICE`
- [ ] Resources, nodeSelector, tolerations, affinity

### Phase 2: Networking
- [ ] `LoadBalancer` with `loadBalancerIP` and annotations (MetalLB / Cilium LB-IPAM)
- [ ] `externalTrafficPolicy: Local` to keep client IPs
- [ ] Ports 80 and 443; `hostNetwork` alternative

### Phase 3: Config and observability
- [ ] Cache values: diskSize, memSize, maxAge, sliceSize, minFreeDisk
- [ ] `upstreamDns`, `cacheDomains`, per-service overrides via ConfigMap
- [ ] ServiceMonitor and Grafana dashboard ConfigMap, both opt-in
- [ ] `helm test` hook curling the heartbeat endpoint

### Phase 4: Storage guidance
- [ ] Document the Longhorn trap: a replicated volume is the wrong shape for a cache
- [ ] Support local PV / hostPath with affinity, single-replica Longhorn, and NFS/iSCSI

## Technical Decisions
- One Kubernetes object per template file, named `resourcetype-name.yaml`.
- The app resolver is used for upstreams regardless of `dnsPolicy`, so cluster DNS settings cannot
  reintroduce the resolution loop.

## Dependencies
- Requires: TASK-15, TASK-13, TASK-18
- Blocks: TASK-23

## Completion Checklist
- [ ] `ct lint` and `ct install` green on kind
- [ ] `helm unittest` covers the templates
- [ ] `helm test` passes against a real install
- [ ] Ten values or fewer for the homelab case
