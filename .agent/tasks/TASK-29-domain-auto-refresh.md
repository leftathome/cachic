# TASK-29: cache-domains auto-refresh with hot reload

## Context
Milestone: M4 | Requirements: FR-61, G4

`uklans/cache-domains` changes as CDNs move. An operator should not have to redeploy to keep
caching Steam.

## Implementation Plan
- [ ] Periodic refresh from the upstream repo with ETag caching
- [ ] Validate the fetched list before applying; a malformed refresh never replaces a good list
- [ ] Hot reload via `SIGHUP` and via the admin API
- [ ] `arc-swap` for the live matcher so reload does not stall the serving path
- [ ] Metric and log on refresh success, failure and no-change
- [ ] Refresh fully disableable for air-gapped installs; bundled snapshot remains the fallback

## Technical Decisions
- Validation before application is the whole point. An automatic update path that can break the
  cache is worse than manual updates.
- Hot swap is pointer-swap, not lock-and-rebuild; a reload must not show up in p99.

## Dependencies
- Requires: TASK-08, TASK-19
- Blocks: v1.0

## Completion Checklist
- [ ] Refresh applies a changed list without dropping connections
- [ ] Malformed list rejected with the previous list still serving
- [ ] Air-gapped mode verified with no outbound requests
- [ ] Reload invisible in latency metrics
