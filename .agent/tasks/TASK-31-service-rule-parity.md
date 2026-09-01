# TASK-31: Per-service rule parity review

## Context
Milestone: M4 | Requirements: G1, FR-21; Appendix C

G1 is functional parity for every service in `cache-domains`. This is the task that actually
checks that claim service by service instead of assuming it.

## Implementation Plan
- [ ] Enumerate every service in `cache-domains` and monolithic's config for it
- [ ] For each: compare key normalisation, query handling, host inclusion, path rewrites,
      include/exclude rules, upstream scheme
- [ ] Capture real request samples per service where possible (Steam, Epic, Blizzard, Riot,
      Windows Update, Xbox, PlayStation, Nintendo)
- [ ] Fixture test per service asserting our key equals the key monolithic would compute
- [ ] Record intentional divergences with reasons

## Technical Decisions
- Parity is asserted per service with a test, not asserted globally with a paragraph.
- Where monolithic's behaviour looks like a bug, we match it anyway in v1 and file an upstream
  issue. Divergence surprises migrators.

## Dependencies
- Requires: TASK-08, TASK-14
- Blocks: v1.0

## Completion Checklist
- [ ] Every service reviewed and marked pass or divergent
- [ ] Fixture test per service
- [ ] Divergences documented in the migration guide
- [ ] Rule fixes contributed upstream to `cache-domains` where relevant
