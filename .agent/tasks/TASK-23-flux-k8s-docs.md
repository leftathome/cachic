# TASK-23: Flux example and Kubernetes documentation

## Context
Milestone: M3 | Requirements: G6; plan section 7.3, section 8

The primary persona runs Talos + Flux. Their install path has to be a committed example, not a
paragraph telling them to figure out the OCIRepository themselves.

## Implementation Plan
- [ ] `deploy/flux/`: `OCIRepository` for the chart, `HelmRelease` with values, `Kustomization`
      that also installs the ServiceMonitor
- [ ] Values for LB IP and storage class called out as the two things everyone changes
- [ ] Kubernetes docs: LB IP allocation, `externalTrafficPolicy: Local`, storage choices and their
      trade-offs, `dnsConfig`, upgrades that reuse the data volume
- [ ] External Secrets note for the optional admin token

## Technical Decisions
- One object per file, `resourcetype-name.yaml`.
- The docs state plainly that a replicated Longhorn volume will disappoint, and why. Operators
  discovering that from a benchmark is worse than reading it.

## Dependencies
- Requires: TASK-22
- Blocks: M3 exit criteria

## Completion Checklist
- [ ] Example applies cleanly on the Talos cluster via Flux
- [ ] Upgrade reuses the data volume without a refill
- [ ] Docs reviewed against an actual first-time install
