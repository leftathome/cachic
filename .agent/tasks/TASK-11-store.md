# TASK-11: Store - foyer wrapper, slice codec, object index

## Context
Milestone: M1 | Requirements: FR-40, FR-41, FR-42, FR-43, FR-44, FR-45; ADR 0003, ADR 0004

The capacity tier and the correctness boundary. Slices are self-describing so the index is a
rebuildable acceleration structure - a bug that inverts that relationship serves wrong bytes.

## Implementation Plan
### Phase 1: Trait boundary
- [ ] `store::Store` trait isolating foyer, so the fallback design stays cheap (risk table,
      plan section 10)

### Phase 2: Slice codec
- [ ] Header: magic, slice_size, total_len, etag, last_modified, content_type, generation,
      xxh3 of payload
- [ ] Encode/decode round-trip property tests
- [ ] Verify checksum on read (configurable); drop and refetch corrupt slices, never serve them

### Phase 3: foyer wrapper
- [ ] `HybridCache<SliceKey, SliceValue>`, memory tier `CACHE_MEM_SIZE`, disk tier
      `CACHE_DISK_SIZE` on `CACHE_DATA_DIR`
- [ ] Recency/frequency-aware eviction (S3-FIFO or LRU class), non-blocking on the serving path
- [ ] Direct IO as a tunable, defaulted from TASK-04 findings
- [ ] **Do not use foyer's defaults**: minimum 2 flushers and a 64 MiB buffer pool. With 1 flusher
      and a 16 MiB pool, a 10 Gbit fill silently loses 10% of slices (docs/benchmarks/m0). Prefer
      4 flushers and 128 MiB for headroom.
- [ ] Surface `storage_queue_channel_overflow` and `storage_block_engine_enqueue_skip` so a cache
      that has stopped caching is visible

### Phase 4: Object index
- [ ] `redb` table `object_id -> {key, len, validators, gen, no_ranges, created, last_seen}`
- [ ] `last_seen` updated at most hourly per object
- [ ] Prune by `CACHE_MAX_AGE`
- [ ] Repair the index from a slice header when an entry is missing but a slice is present

### Phase 5: Recovery
- [ ] Serve hits within seconds of start; rebuild the full index in the background
- [ ] No dependency on a clean shutdown

## Technical Decisions
- The index is never authoritative. Any code path that resolves an index/slice disagreement in
  favour of the index is a bug.
- Slice presence is all-or-nothing: a torn write must read back as absent, not as short.

## Dependencies
- Requires: TASK-07, ADR 0003, ADR 0004
- Blocks: TASK-12

## Completion Checklist
- [ ] Codec round-trip property tests
- [ ] Crash-safety test: kill mid-write, confirm no partial slice is readable
- [ ] Index rebuild from slices alone, with the index file deleted
- [ ] Eviction holds the cap under sustained write load without stalling reads
- [ ] 100% of slices retained at a 10 Gbit fill rate (1192 MiB/s), asserted by a test
