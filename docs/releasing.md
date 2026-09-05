# Releasing

```sh
git tag -s v0.1.0 -m "v0.1.0"
git push origin v0.1.0
```

The tag triggers everything. `workflow_dispatch` runs the same pipeline without publishing, for
checking a change to the pipeline itself.

## What a tag produces

- **A multi-arch image** on `ghcr.io`, built on native amd64 and arm64 runners rather than under
  QEMU. A release build with LTO under emulation takes long enough to make releasing painful, and
  both architectures are available.
- **A cosign signature**, keyless. There is no long-lived private key to protect or rotate.
- **An SBOM** in SPDX JSON, attached to the image as an attestation and to the release.
- **A Helm chart** pushed to the OCI registry, versioned to match the application. A chart that
  lags its app is a support question waiting to happen.
- **Binaries** for linux amd64 and linux arm64, dynamically linked against glibc — see
  [known limitations](known-limitations.md) for the floor and why they are not static.
- **A `cachic-tools` tarball** per architecture, holding `bench`, `soak`, `loadtest` and `origin`.
  These are the harnesses the test plan's measurement sections call for. They are separate from
  the main tarball because they generate traffic and synthetic data and have no business on a
  cache serving real clients.
- **A GitHub release** with a changelog generated from conventional commits.

## Two things about this pipeline that are not obvious

**The SBOM only means something because the binary carries its own dependency list.** syft scans
the published image, and the image is distroless with a single stripped binary in it — so left to
itself syft catalogues the Debian base layer and reports about a dozen OS packages, none of them a
Rust crate. v0.1.0-rc3 shipped exactly that: an SBOM that looked complete and said nothing about
the 232 crates cachic is built from. The binary is now built with `cargo auditable`, which embeds
the resolved dependency graph in a `.dep-v0` section that syft, trivy and `cargo audit` all read.
`strip` is told to keep that section and the build fails if it is gone, because the failure mode
is silent.

**The GitHub release is created as a draft and published afterwards.** This repository has
immutable releases enabled, so a published release accepts no further uploads, and a prerelease
publishes the moment it is created rather than waiting behind `release.prereleased`. Creating it
published raced its own asset uploads: rc2 published a release with nothing attached, and the tag
could not be reused because ref creation is restricted. Draft, attach, publish, then assert all
four tarballs actually arrived.

## Why the pipeline re-runs the whole gate

A tag is not a branch. It can point at a commit that never ran branch CI, or at one where CI
passed before a dependency advisory landed. The `verify` job runs fmt, clippy, the full test suite
and the performance gate again, and everything else depends on it.

## Not musl

See [known limitations](./known-limitations.md) for the full write-up.

The plan called for static musl binaries. foyer 0.22 does not compile for musl: `foyer-storage`
guards its macOS `ioctl` branch with `cfg!(target_os = "macos")` - a runtime boolean - rather than
`#[cfg(...)]`, so the Darwin call is compiled on every unix target and only typechecks against
glibc, where `libc::Ioctl` is `u64`. The upstream fix is one line.

Until it lands, binaries are glibc and the image uses `distroless/cc`. This is the outstanding
part of FR-73.

## Enabling the documentation site

The book builds on every push and is uploaded as a Pages artifact, but publishing is off until
Pages is enabled on the repository — turning it on creates a public website, which is a decision
rather than a default:

```sh
gh api -X POST repos/leftathome/cachic/pages -f 'build_type=workflow'
```

Until then the `Publish docs` job reports its own failure without holding the build red.

## Verifying a release

```sh
cosign verify ghcr.io/leftathome/cachic:0.1.0 \
  --certificate-identity-regexp 'https://github.com/leftathome/cachic/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com

cosign verify-attestation ghcr.io/leftathome/cachic:0.1.0 --type spdxjson \
  --certificate-identity-regexp 'https://github.com/leftathome/cachic/.*' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

## Versioning and stability

SemVer. Two surfaces are covered by the stability promise and must not change incompatibly within
a major version:

- **Environment variable names and meanings.** An operator's configuration is not ours to break,
  and it is the thing most likely to be templated into something we cannot see.
- **The admin API.** Once documented, it is a contract.

The on-disk store format is *not* covered, but it is guarded: `CACHE_DATA_DIR/CONFIG` records the
format version, and a mismatch refuses to start rather than reinterpreting existing slices.

## Secrets

The pipeline uses `GITHUB_TOKEN` for the registry and keyless OIDC for signing. **There are no
long-lived secrets to manage**, which is deliberate — nothing to leak, rotate or accidentally
commit. If the canonical registry ever moves off GHCR, its credentials belong in 1Password or
Vault and should be synchronised into CI at build time, never committed.
