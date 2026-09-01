# TASK-13: Telemetry - logs, metrics, health

## Context
Milestone: M1 | Requirements: FR-50, FR-51, FR-53, G3

Observability is a headline reason this project exists. Every dashboard in the lancache ecosystem
is a log tailer because nginx gave them nothing else; we ship metrics from day one.

## Implementation Plan
### Phase 1: Logs
- [ ] `tracing-subscriber` JSON to stdout, level from config
- [ ] Per-request event: client IP, service, host, path, range, status, bytes, cache status,
      upstream time
- [ ] Access log as a dedicated `tracing` target with its own formatter

### Phase 2: Metrics
- [ ] `metrics` + `metrics-exporter-prometheus` on the admin port
- [ ] Requests, bytes served and bytes fetched by service and cache status
- [ ] Upstream latency histograms; in-flight fetches
- [ ] Store size, entries, evictions, checksum failures; connection counts
- [ ] foyer's own metrics surfaced through the same registry
- [ ] Bounded label cardinality - no per-URL labels, ever

### Phase 3: Health
- [ ] `/healthz` - process up
- [ ] `/readyz` - store initialised and listeners bound

## Technical Decisions
- Cardinality is a production hazard, not a style preference. Service names are bounded; URLs are
  not. Enforce that in review.
- New behaviour ships with a metric, or it is invisible in the field.

## Dependencies
- Requires: TASK-07, TASK-11
- Blocks: TASK-25 (benchmarks read these), TASK-30 (dashboard)

## Completion Checklist
- [ ] `/metrics` scrapes cleanly; a Prometheus config is checked in
- [ ] Cardinality test asserting no unbounded labels
- [ ] `/readyz` fails while the store is initialising and passes after
- [ ] Log schema documented
