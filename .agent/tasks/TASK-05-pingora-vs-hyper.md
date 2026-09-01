# TASK-05: Pingora vs hyper evaluation note

## Context
Milestone: M0 | Requirements: plan decision 0.3

Default is hyper. Pingora wins only if its `ProxyHttp::request_filter` short-circuiting plus its
upstream connectors give a cleaner slice orchestrator than hyper + reqwest, and its Linux-first
stance is acceptable. This task closes the question with evidence so it stops being reopened.

## Implementation Plan
- [ ] Sketch the slice orchestrator against Pingora's request model and identify where the
      one-downstream-request-to-N-upstream-responses shape fights it
- [ ] Assess `pingora-cache`: storage is test-grade `MemCache`, APIs documented as experimental -
      confirm current state rather than trusting the plan's snapshot
- [ ] Compare connection pooling, timeouts and resolver control against reqwest
- [ ] Note platform support (Linux first class; macOS best-effort) against our tiers
- [ ] Write the recommendation with the reasoning, not just the verdict

## Technical Decisions
- This is a written note, not a second prototype. If the note cannot reach a conclusion without
  building, that itself is the answer: stay on hyper and revisit in 1.x.

## Dependencies
- Requires: TASK-03 (the hyper path exists to compare against)
- Blocks: TASK-06 (ADR 2)

## Completion Checklist
- [ ] Recommendation recorded with reasoning
- [ ] Feeds ADR 2 (HTTP layer)
- [ ] Claims about Pingora verified against current docs, with dates
