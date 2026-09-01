# TASK-04: M0 measurements

## Context
Milestone: M0 | Requirements: NFR-1, NFR-2, NFR-5, NFR-6; plan section 4 (M0 exit criteria)

The go/no-go on Rust + foyer is a measurement, not an opinion. This task produces the numbers that
the ADRs cite and that the risk table in plan section 10 is checked against.

## Implementation Plan
### Phase 1: Store microbenchmarks
- [ ] foyer write and read throughput with 1 MiB entries, memory tier and disk tier separately
- [ ] RAM per indexed entry at 1M and 10M entries (this is the lancache
      "1 MB of CACHE_MEM_SIZE per ~8 GB of slices" rule of thumb, restated for foyer)
- [ ] Direct IO vs page cache: is double buffering costing us throughput or RSS?
- [ ] Allocator comparison: `mimalloc` vs system

### Phase 2: End-to-end
- [ ] Hit throughput through the spike proxy with 8 concurrent clients
- [ ] p50/p99 TTFB for RAM-tier and disk-tier hits
- [ ] Cold-fill overhead versus fetching from `mockcdn` directly
- [ ] RSS under sustained load, checked against NFR-5

### Phase 3: Recovery
- [ ] Recovery time for a large cache (target: 500 GB; scale down and extrapolate if the dev host
      cannot hold it, and say so in the report)
- [ ] Time to first hit after restart, against NFR-6

### Phase 4: Report
- [ ] Raw CSV plus the exact commands, committed under `docs/benchmarks/m0/`
- [ ] Hardware and version table alongside every number

## Technical Decisions
- **Every number is only valid for the hardware it was taken on.** The M0 exit criterion
  (>= 8 Gbps with 8 clients) is specified on the amd64 NUC with NVMe. Measurements taken on any
  other host are a provisional signal, and the report must label them as such.
- Benchmark data directories must live on native Linux storage. A `/mnt/c` DrvFs path under WSL2
  measures the Windows filesystem bridge, not the cache.
- Prefer measuring one variable at a time; a single "it felt fast" run is not a result.

## Dependencies
- Requires: TASK-03
- Blocks: TASK-06 (ADRs cite these numbers), M0 go/no-go

## Completion Checklist
- [ ] Every measurement in the Implementation Plan has a number or a written reason it is missing
- [ ] Results committed with commands and hardware described
- [ ] Explicit statement of whether the Rust + foyer bet holds
- [ ] Outstanding runs (NUC, Synology NFS) listed as named follow-ups
