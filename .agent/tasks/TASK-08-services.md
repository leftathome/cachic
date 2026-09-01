# TASK-08: Services - cache-domains, matcher, key normalisation

## Context
Milestone: M1 | Requirements: FR-02, FR-21, FR-61, G1, G4

Functional parity with monolithic means reproducing its per-service key behaviour for every
service in `uklans/cache-domains`. This is the module where "parity" is won or lost.

## Implementation Plan
### Phase 1: Domain list
- [ ] Bundle a `cache-domains` snapshot at build time
- [ ] Parser with fixtures from the real repo
- [ ] Validate before applying (a malformed refresh must never replace a good list)

### Phase 2: Matcher
- [ ] Compiled host matcher: exact + wildcard, case-insensitive, port stripped
- [ ] Benchmark the lookup; it is on every request

### Phase 3: Key normalisation
- [ ] `object_id = blake3(identifier || normalised_key)[..16]`
- [ ] Default: drop the query string, exclude the host
- [ ] Per-service rules: keep-query, include-host, path rewrites, include/exclude regexes
- [ ] Ship rules reproducing monolithic's current per-service behaviour

### Phase 4: Unmatched hosts
- [ ] 404 by default; `passthrough` mode optional (FR-02) and off by default (FR-64)

## Technical Decisions
- Rules are data, not code, so a new service is a config change and not a release.
- Key normalisation is tested against fixtures captured from real client traffic, not invented
  URLs; Steam, Windows Update and Blizzard have the awkward cases.

## Dependencies
- Requires: TASK-07
- Blocks: TASK-12 (orchestrator), TASK-31 (parity review)

## Completion Checklist
- [ ] Property tests for normalisation
- [ ] Fixture tests per service against captured URLs
- [ ] Matcher benchmark recorded
- [ ] Unmatched-host behaviour tested in both modes
