# 0007. Access-log compatibility with lancache tooling

- **Status**: Accepted
- **Date**: 2026-09-01
- **Context**: FR-52, G4

## Context

Every dashboard in the lancache ecosystem - LANCache Manager, DeveLanCacheUI, lancache-ui - is a
log tailer, because nginx gave them nothing else to consume. Users migrating from monolithic keep
their dashboards or they do not migrate.

## Decision

Emit the `lancache` access-log format field-for-field with monolithic's `cachelog`, as an optional
output configurable independently of the JSON log, to a file or stdout.

Treat it explicitly as a **compatibility shim, not the observability story.** `/metrics` is the
supported path, and it is one of the reasons this project exists.

## Consequences

Two `tracing` targets with separate formatters: structured JSON for operators and log pipelines,
lancache format for existing dashboards. Neither is derived from the other.

What does not carry over must be documented as prominently as what does. LANCache Manager reads
nginx's hashed cache directory to browse and purge; our store is a different shape and that feature
cannot work against us. The answer is the admin API (FR-54), and the right move is to offer
LANCache Manager upstream support for it rather than to contort our storage into nginx's layout -
which the PRD already rules out as N5.

Field-for-field means verified against the real tool, not against a reading of its source, and
covered by a fixture test so the format cannot drift silently.

## What would overturn this

LANCache Manager adopting a native backend for our admin API, at which point the log shim becomes
legacy support for older installs rather than the migration path.
