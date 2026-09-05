# TASK-34: Disk-tier read amplification and the warm-serving gap

**Status**: Open
**Milestone**: post-rc2
**Origin**: measured while fixing the rc1 findings; see `docs/benchmarks/rc2-dev/README.md`

## Context

cachic is at parity with `lancachenet/monolithic` or ahead of it until the working set is several
times the RAM tier. Past that point, on warm serving, it gives up about 12%:

| Working set | State | cachic | monolithic |
|---|---|---|---|
| 3 GiB | warm | 5.21 Gbps | 4.56 Gbps |
| 6 GiB | warm | 4.56 Gbps | 5.19 Gbps |

The obvious suspect is read amplification on the disk tier. On a warm read-only pass foyer
reported 41.8 GiB read from disk to serve 26.7 GiB to clients — roughly 1.6x. Per-stage timings
from the same run put nearly all of a disk-tier hit in the read itself:

| Stage | Mean |
|---|---|
| storage hit, total | 3904 us |
| disk read | 3241 us (83%) |
| deserialize + checksum | 588 us (15%) |

**This is a suspicion, not a diagnosis.** The correlation is suggestive and the mechanism is not
established. It was measured on a virtualised disk in WSL2, where a 3.2 ms read for roughly a
slice is slow enough to be an artefact of the environment rather than of cachic.

## What would settle it

1. Repeat the matched warm comparison on real NVMe. If the gap does not survive, it was the
   virtualised disk and this task closes.
2. If it does survive, establish whether the 1.6x is block granularity — `block_bytes` is
   `max(DEFAULT_BLOCK_SIZE, slice_size * 4)`, so a read may pull a whole block for one slice —
   or foyer reading more than it needs per entry.
3. Test `CACHE_DIRECT_IO=true`. It is now settable. The M0 note says buffered reads measured about
   twice direct on the development host, so this likely makes things worse; it is worth one
   measurement to confirm rather than assume.

## Do not

- Do not tune `CACHE_FLUSHERS` or `CACHE_BUFFER_POOL` for this. Those govern the write path and
  the pass in question wrote nothing.
- Do not raise the RAM tier as the fix. A tier sweep at a fixed working set showed 512m, 2g and 4g
  inside one noise band, and the tier is unreclaimable memory where the page cache is not.

## Related

- TASK-25 (benchmarks) — produces the nginx floor constant the performance gate still lacks.
- `docs/rc-test-plan.md` section A, and reporting item 5.
