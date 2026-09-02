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
- **An SBOM** in SPDX JSON, attached to the image as an attestation.
- **A Helm chart** pushed to the OCI registry, versioned to match the application. A chart that
  lags its app is a support question waiting to happen.
- **Static binaries** for linux amd64, linux arm64 and macOS arm64.
- **A GitHub release** with a changelog generated from conventional commits.

## Why the pipeline re-runs the whole gate

A tag is not a branch. It can point at a commit that never ran branch CI, or at one where CI
passed before a dependency advisory landed. The `verify` job runs fmt, clippy, the full test suite
and the performance gate again, and everything else depends on it.

## Not musl

The plan called for static musl binaries. foyer 0.22 does not compile for musl: `foyer-storage`
guards its macOS `ioctl` branch with `cfg!(target_os = "macos")` - a runtime boolean - rather than
`#[cfg(...)]`, so the Darwin call is compiled on every unix target and only typechecks against
glibc, where `libc::Ioctl` is `u64`. The upstream fix is one line.

Until it lands, binaries are glibc and the image uses `distroless/cc`. This is the outstanding
part of FR-73.

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
