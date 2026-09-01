# TASK-15: Packaging v0.1 - image and compose

## Context
Milestone: M1 | Requirements: FR-63, FR-70, FR-72, NFR-8; plan section 7.1

M1's exit criteria include a non-root image running on amd64 and arm64. The container is the
artefact users actually run, so it is tested as a container.

## Implementation Plan
### Phase 1: Image
- [ ] Multi-stage Dockerfile with `cargo-chef` for layer caching
- [ ] `--release` with LTO and `codegen-units=1`; static musl binary
- [ ] Final stage `gcr.io/distroless/static` or `scratch`, non-root user
- [ ] Document `CAP_NET_BIND_SERVICE` for hosts binding port 80 without a port map
- [ ] OCI labels; image under 40 MB compressed (NFR-8)

### Phase 2: Compose
- [ ] `deploy/compose/` example mirroring the lancache quickstart: `lancache-dns` + cache
- [ ] A `ci` profile wiring `mockcdn` for integration runs

### Phase 3: Verification
- [ ] Runs as non-root on amd64 and arm64
- [ ] Data volume survives a container replacement

## Technical Decisions
- Code changes reach a container by rebuilding the image. Never copy a binary into a running
  container - it produces an artefact nobody can reproduce.
- Multi-arch build and signing are TASK-26; this task only needs both arches to run.

## Dependencies
- Requires: TASK-09, TASK-11, TASK-13
- Blocks: TASK-22 (chart), TASK-25 (benchmarks run the image)

## Completion Checklist
- [ ] Image builds reproducibly from a clean tree
- [ ] Non-root verified on both arches
- [ ] Compose quickstart works end to end
- [ ] Image size within NFR-8
