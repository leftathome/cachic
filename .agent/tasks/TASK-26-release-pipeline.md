# TASK-26: Release pipeline

## Context
Milestone: M3 | Requirements: FR-70, FR-73; plan section 6

Signed, reproducible, multi-arch artefacts. This is also where secrets management gets settled, so
it gets done deliberately rather than under tag-day pressure.

## Implementation Plan
### Phase 1: Versioning
- [ ] `release-plz` or `cargo-release` for version bumps
- [ ] `git-cliff` changelog from conventional commits
- [ ] SemVer with a written stability policy for env vars and the admin API

### Phase 2: Artefacts
- [ ] Binaries via `cargo-dist`: linux amd64/arm64 musl, macOS
- [ ] Multi-arch images via `docker buildx`; prefer native runners per arch over QEMU
- [ ] Tags: `vX.Y.Z`, `vX.Y`, `latest`, `sha-...`

### Phase 3: Supply chain
- [ ] `cosign` keyless signing
- [ ] SBOM via `syft`, attached to the release
- [ ] Chart published to an OCI registry plus a `chart-releaser` pages index

### Phase 4: Secrets
- [ ] Registry and signing credentials stored in 1Password or Vault and synchronised into CI
- [ ] No unencrypted secret ever committed; verify with a secret scanner in CI

## Technical Decisions
- Native per-arch runners over QEMU: QEMU builds of a Rust release with LTO are slow enough to
  make releases painful, and the cluster already has both arches.
- Signing is keyless so there is no long-lived private key to protect.

## Dependencies
- Requires: TASK-02, TASK-15, TASK-22
- Blocks: M3 exit criteria, v1.0

## Completion Checklist
- [ ] A tag produces signed multi-arch images, binaries, chart and changelog
- [ ] Signature and SBOM verify from a clean machine
- [ ] Secret scanner in CI
- [ ] Stability policy written
