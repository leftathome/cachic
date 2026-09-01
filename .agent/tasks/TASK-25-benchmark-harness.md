# TASK-25: Benchmark harness and parity report

## Context
Milestone: M3 | Requirements: G2, NFR-1, NFR-2, NFR-3; plan section 9

"Performance parity or better" is the second goal in the PRD, and the M3 exit criterion is a
published report showing parity on every scenario. A benchmark nobody can reproduce proves nothing.

## Implementation Plan
### Phase 1: Environment
- [ ] amd64 NUC (NVMe) as cache host, second host as 10 GbE client
- [ ] `mockcdn` behind `tc netem` (1 Gbps, 20 ms) as the WAN origin
- [ ] Same data volume mounted into `lancachenet/monolithic` and cachic in alternating runs
- [ ] Identical `cache-domains` for both

### Phase 2: Scenarios
- [ ] S1 warm single client, full 20 GB object - Gbps, CPU %, RSS
- [ ] S2 warm 32 clients, same object - aggregate Gbps, p50/p99 TTFB, CPU per Gbps
- [ ] S3 warm 32 clients, 32 distinct objects - plus disk IOPS/MB/s
- [ ] S4 cold fill, 8 clients, same object - upstream bytes should be about one object size
- [ ] S5 random 64 KiB-8 MiB ranges into 5 GB objects - hit ratio, amplification, p99
- [ ] S6 restart with 500 GB cached - time to first hit, time to full index
- [ ] S7 eviction at cap, 24 h mixed replay - hit ratio, eviction rate, latency stability

### Phase 3: Report
- [ ] Hardware, versions, raw CSV and the exact commands under `docs/benchmarks/`
- [ ] Alternating-run methodology stated so results are not accused of ordering bias

## Technical Decisions
- Alternating runs on the same volume, not two volumes, so disk layout is not a confound.
- Publish losses as well as wins. A report that only shows favourable scenarios is not a parity
  claim.

## Dependencies
- Requires: TASK-14, TASK-15
- Blocks: M3 exit criteria

## Completion Checklist
- [ ] All seven scenarios run against both engines
- [ ] Raw data and commands committed
- [ ] NFR-1, NFR-2, NFR-3 assessed explicitly
- [ ] Any scenario where we lose is documented with a plan or an accepted trade-off
