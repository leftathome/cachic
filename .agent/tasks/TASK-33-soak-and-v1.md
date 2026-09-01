# TASK-33: Soak, definition of done, v1.0 release

## Context
Milestone: M4 | Requirements: NFR-7; Appendix C

The final gate. Everything in Appendix C is checked here, and v1.0 ships or the gap is named.

## Implementation Plan
### Phase 1: Soak
- [ ] 7-day run on the homelab with real clients and checksum verification on
- [ ] Zero integrity failures (NFR-7); any failure resets the clock
- [ ] Memory, disk headroom and latency stable across the window

### Phase 2: Real-world verification
- [ ] SteamPrefill, Epic and Battle.net prefill runs complete through the proxy
- [ ] LANCache Manager log features working

### Phase 3: Definition of done
- [ ] All PRD P0 and P1 requirements implemented with tests
- [ ] Benchmark report published showing parity on S1-S7
- [ ] Chart installed via Flux on Talos; Grafana dashboard live
- [ ] Docs site complete; CHANGELOG and signed artefacts for amd64 and arm64

### Phase 4: Release and handover
- [ ] Tag v1.0.0
- [ ] Announce
- [ ] Open issues for the 1.x backlog: revalidation (FR-23), nginx cache import (FR-47),
      OpenTelemetry traces (FR-55), raw block device, sharding

## Technical Decisions
- The soak is not a formality. It is the only test that runs long enough to find slow leaks,
  eviction pathologies and index drift.
- Ship with named gaps rather than quietly redefining done.

## Dependencies
- Requires: every preceding task
- Blocks: nothing - this is the milestone

## Completion Checklist
- [ ] 7-day soak clean
- [ ] Every Appendix C item checked
- [ ] v1.0.0 tagged with signed artefacts
- [ ] 1.x backlog filed
