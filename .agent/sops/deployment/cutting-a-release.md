# SOP: Cutting a release

**Category**: deployment
**Created**: 2026-09-05
**Last Updated**: 2026-09-05

---

## Context

**When to use**: tagging any `v*.*.*`, including release candidates.

**Problem it solves**: v0.1.0-rc1 through rc5 burned four tags on defects that only a real release
exercises. Every one of them is now either fixed in the workflow or listed under Traps below. The
expensive part is not the fix, it is that **a tag cannot be reused** — see Trap 2 — so each defect
costs a version number.

**Prerequisites**: push access, a clean tree on `main`, and CI green on `main` before you start.

---

## Procedure

### 1. Gate locally first

CI runs the same checks, but a failure here costs 30 seconds instead of a tag.

```sh
export CARGO_TARGET_DIR=/root/.cache/cachic-target CARGO_INCREMENTAL=0
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
cargo deny check
helm lint charts/cachic && helm template t charts/cachic | kubeconform -strict -summary
```

`CARGO_INCREMENTAL=0` matters on this machine: incremental artefacts reached 45 GB and filled the
WSL disk. See Trap 6.

### 2. Bump the version in all four places

They drift independently and nothing checks them against each other:

| File | Field |
|---|---|
| `Cargo.toml` | `version` (workspace) |
| `charts/cachic/Chart.yaml` | `version`, `appVersion`, and the `artifacthub.io/images` annotation |
| `.agent/tasks/TASK-INDEX.md` | the TASK-33 status line |
| `Cargo.lock` | via `cargo update -w` |

```sh
cargo update -w
git diff --stat   # Cargo.lock should show only the version change
```

### 3. Commit, push, and let CI go green on main

```sh
git commit -am "chore(release): X.Y.Z"
git push origin main
gh run list --branch main --limit 1
```

Wait for it. Tagging a red main means the release job re-discovers the failure ten minutes later.

### 4. Tag and push

Annotated, with a message that says what changed and what is still broken.

```sh
git tag -a vX.Y.Z -F - <<'EOF'
vX.Y.Z
...
EOF
git push origin vX.Y.Z
```

### 5. Watch the release workflow

```sh
RUN=$(gh run list --limit 6 --json databaseId,name,headBranch \
  -q '.[] | select(.name=="Release" and .headBranch=="vX.Y.Z") | .databaseId' | head -1)
until [ "$(gh run view "$RUN" --json status -q .status)" = completed ]; do sleep 90; done
gh run view "$RUN" --json conclusion,jobs -q '.conclusion, (.jobs[] | "\(.conclusion)\t\(.name)")'
```

Eight jobs must be green: Verify, Chart, Image (amd64), Image (arm64), Binaries (x86_64),
Binaries (aarch64), Manifest/signature/SBOM, GitHub release.

### 6. Verify the artefacts, not the logs

**This is the step that matters.** rc3's pipeline was entirely green and shipped an SBOM that
described nothing. A green job means the command exited zero, not that it produced what you meant.

```sh
# gh is a snap and has a private /tmp - download somewhere it can write (Trap 5)
gh release download vX.Y.Z --repo leftathome/cachic --dir ~/relcheck

# every expected asset present, and marked prerelease for an rc
gh release view vX.Y.Z --json isPrerelease,assets -q '.isPrerelease, (.assets[].name)'

# the SBOM describes the application, not just the base image
python3 -c "
import json; d=json.load(open('$HOME/relcheck/sbom.spdx.json'))
n={p['name'] for p in d['packages']}
print(len(d['packages']), 'packages'); print('tokio' in n and 'foyer' in n)"

# the binary runs on the oldest supported glibc, and needs nothing newer than 2.36
tar -xzf ~/relcheck/cachic-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz -C ~/relcheck
objdump -T ~/relcheck/cachic | grep -oE 'GLIBC_[0-9.]+' | sort -uV | tail -1
~/relcheck/cachic --version

# image labels trace to the commit
docker pull ghcr.io/leftathome/cachic:X.Y.Z
docker inspect ghcr.io/leftathome/cachic:X.Y.Z \
  --format '{{index .Config.Labels "org.opencontainers.image.revision"}}'
```

For the OCI index annotations, query the registry directly — `docker manifest inspect` normalises
the index and will show zero annotations even when eight are present:

```sh
TOKEN=$(curl -s "https://ghcr.io/token?scope=repository:leftathome/cachic:pull&service=ghcr.io" \
  | python3 -c "import sys,json;print(json.load(sys.stdin)['token'])")
curl -sL -H "Authorization: Bearer $TOKEN" \
  -H "Accept: application/vnd.oci.image.index.v1+json" \
  https://ghcr.io/v2/leftathome/cachic/manifests/X.Y.Z | python3 -m json.tool | head -20
```

---

## Traps

Each of these cost a tag.

### Trap 1: the release publishes with no assets

**Symptom**: `Cannot upload asset ... to an immutable release`, and a published release with
nothing attached. Hit on rc2.

**Cause**: the repository has immutable releases enabled, so a published release accepts no
further uploads — and a *prerelease* publishes the moment it is created rather than waiting behind
`release.prereleased`. Creation raced its own uploads.

**Fix (already in the workflow)**: `draft: true`, then `gh release edit --draft=false`, then assert
all four tarballs are attached. Do not remove that assertion; an empty release looks finished.

### Trap 2: you cannot re-use the tag

**Symptom**: `GH013: Cannot create ref due to creations being restricted` when re-pushing a tag you
deleted.

**Cause**: a repository ruleset restricts ref creation, and a deleted tag stays deleted.

**Fix**: there isn't one. Bump to the next rc and move on. Delete the broken GitHub release
(`gh release delete vX.Y.Z --yes`) so it does not sit there looking real.

### Trap 3: the manifest list fails on a value containing spaces

**Symptom**: `failed to parse source "HTTP-slice" ... must be lowercase`. Hit on rc4.

**Cause**: annotation values were expanded unquoted into `docker buildx imagetools create`, and
`org.opencontainers.image.description` contains spaces, so word splitting produced a bogus image
reference.

**Fix (already in the workflow)**: build the argument list as a bash array. If you touch that step,
test the argument construction against the real `steps.meta.outputs.json` shape before tagging.

### Trap 4: an SBOM that describes the base image only

**Symptom**: `sbom.spdx.json` lists about a dozen Debian packages and no Rust crates. Shipped in
rc3.

**Cause**: syft scans the published image, which is distroless with one stripped binary in it. It
catalogues the base layer and stops.

**Fix (already in the Dockerfile)**: `cargo auditable build`, which embeds the dependency graph in
a `.dep-v0` section that syft reads, plus `strip --keep-section=.dep-v0` and a build-time check
that the section survived. Expect ~227 packages, not 12.

### Trap 5: `gh release download` silently downloads nothing

**Symptom**: exit code 0, no files, no error.

**Cause**: `gh` is installed as a snap, and snaps get a private `/tmp`. The files land inside the
snap's namespace.

**Fix**: download to a path under `$HOME`.

### Trap 6: the build cache fills the disk

**Symptom**: Windows C: at 98%, WSL crashes with a bus error.

**Cause**: `target/debug/incremental` reached 45 GB across many test runs; the WSL ext4 lives in a
VHDX on C:, and the VHDX grows and never shrinks on its own.

**Fix**: `CARGO_INCREMENTAL=0` for repeated runs, and `rm -rf $CARGO_TARGET_DIR/debug` when it gets
large. Freeing space inside the VM does **not** return it to C:; that needs
`wsl --shutdown` then `wsl --manage <distro> --set-sparse true` from Windows.

---

## Prevention

- Never claim an artefact is correct because its job was green. Download it and look.
- When adding a step to the release workflow, ask what it produces when it "succeeds" but does
  nothing — an empty SBOM and an assetless release both passed.
- Anything the workflow asserts (glibc floor, asset presence, `.dep-v0`) should fail loudly. If you
  add a new guarantee, add its assertion in the same commit.

---

## Related

- `docs/releasing.md` — the user-facing version of this
- `docs/known-limitations.md` — the glibc floor, and why musl is blocked (foyer#1338)
- `.agent/sops/development/measuring-in-a-container.md`
