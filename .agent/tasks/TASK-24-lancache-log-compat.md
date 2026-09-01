# TASK-24: lancache access-log format and ecosystem compatibility

## Context
Milestone: M3 | Requirements: FR-52, G4

Every existing dashboard - LANCache Manager, DeveLanCacheUI, lancache-ui - is a log tailer. Users
migrating keep their dashboards or they do not migrate.

## Implementation Plan
- [ ] Emit the `lancache` access-log format field-for-field with monolithic's `cachelog`
- [ ] Configurable target: file or stdout, independent of the JSON log
- [ ] Smoke test: run LANCache Manager against our log and confirm its views populate
- [ ] Document what LANCache Manager cannot do against us (it reads nginx's hashed cache directory
      to browse and purge - our store is different) and point at the admin API instead
- [ ] Open the upstream conversation about backend support for our admin API

## Technical Decisions
- This is a compatibility shim, not the observability story. It exists so migration is not a
  cliff; `/metrics` is the supported path.
- Field-for-field means byte-compatible where the tools parse positionally. Verify with the real
  tool, not by reading its source.

## Dependencies
- Requires: TASK-13
- Blocks: M3 exit criteria

## Completion Checklist
- [ ] LANCache Manager populates from our log
- [ ] Differences from monolithic documented in the migration guide
- [ ] Format covered by a fixture test so it cannot drift silently
