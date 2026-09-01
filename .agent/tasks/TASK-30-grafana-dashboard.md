# TASK-30: Grafana dashboard

## Context
Milestone: M4 | Requirements: FR-50, G3; plan section 8

The metrics exist from M1; this makes them legible. It is also the artefact that shows what this
project offers over a log tailer.

## Implementation Plan
- [ ] Dashboard JSON under `dashboards/`, shipped as a ConfigMap by the chart
- [ ] Panels: hit ratio by service, bytes served vs fetched, upstream latency percentiles,
      in-flight fetches, store size and headroom, eviction rate, checksum failures,
      connection counts
- [ ] Alert-worthy signals identified: disk guard engaged, checksum failures non-zero,
      upstream error rate, readiness flapping
- [ ] Screenshot in the docs

## Technical Decisions
- One dashboard that answers "is the cache healthy and is it helping", not twenty panels nobody
  reads. Depth belongs in ad-hoc queries.
- Panels use only bounded-cardinality labels, matching the FR-50 constraint.

## Dependencies
- Requires: TASK-13, TASK-22
- Blocks: v1.0

## Completion Checklist
- [ ] Dashboard imports cleanly into a current Grafana
- [ ] Every panel has data from a real run
- [ ] Shipped by the chart behind `metrics.grafanaDashboard.enabled`
- [ ] Alert signals documented
