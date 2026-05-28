# Releasing agent-tui

This is the runbook for cutting a release. The design behind the
pipeline lives in [`docs/release-rfc.md`](docs/release-rfc.md); read
that if you want to understand *why*. This doc is *how*.

## TL;DR

```
# 1. Land features on main using conventional commits.
git commit -m "feat(selector): cache compiled selectors"
git push

# 2. release-plz opens a Release PR. Review it.
# 3. Merge the Release PR.
# 4. release-plz tags the release. dist builds + signs + publishes.
# 5. Verify the release: `cosign verify-blob` + `slsa-verifier`.
```

The whole pipeline is tag-driven. Pushing a tag matching
`v<MAJOR>.<MINOR>.<PATCH>[-<prerelease>]` triggers the release
workflow. Nothing else does.

## The mental model

```
feature branch  ─► PR  ─► merge to main
                          │
                          ▼
                     release-plz   ─► Release PR (bumps version,
                                       regenerates CHANGELOG.md)
                          │
                          ▼  (you merge the Release PR)
                     git tag v0.2.0  (release-plz creates the tag)
                          │
                          ▼
                     .github/workflows/release.yml  ─► artifacts
                          │
                          ▼
                     GitHub Release  ─► curl installer
                                     ─► Homebrew tap
                                     ─► npm registry
                                     ─► (later) ghcr.io container
```

## Tag format

| Form | Example | What happens |
|---|---|---|
| `v<X>.<Y>.<Z>` | `v0.2.0` | Stable release. Published to all channels. |
| `v<X>.<Y>.<Z>-<kind>.<n>` | `v0.2.0-rc.1`, `v1.0.0-alpha.3` | Prerelease. Published to GitHub Releases marked as prerelease. Homebrew/npm are skipped (those only publish stable). |

The tag MUST match the workspace version in `Cargo.toml`. If they
don't match, `dist` rejects the run. release-plz keeps them in sync
automatically when you use its Release PR flow.

## Cutting a release — the happy path

### 1. Land features on main

Use conventional commit prefixes so release-plz can write a useful
changelog:

| Prefix | What it triggers |
|---|---|
| `feat(scope): ...` | Minor version bump |
| `fix(scope): ...` | Patch version bump |
| `feat!: ...` or `BREAKING CHANGE: ...` in body | Major version bump |
| `docs(...)`, `chore(...)`, `test(...)`, `ci(...)` | No version bump (excluded from changelog) |
| `refactor(...)`, `perf(...)` | Patch version bump |

If a single PR has multiple commits, release-plz uses the highest
bump level among them.

### 2. Wait for the Release PR

release-plz watches `main`. Within a few minutes of any push, it
opens (or updates) a PR titled `chore: release v0.2.0`. The PR
contains:

- A bump to the workspace `version = "..."` in `Cargo.toml`.
- A regenerated `CHANGELOG.md`.
- An updated `Cargo.lock`.

Review the proposed version and changelog. If something's wrong
(missing entry, wrong bump level), fix the underlying commit
messages on main and push — release-plz will rebase its PR.

### 3. Merge the Release PR

Merge via "Squash and merge" or "Rebase and merge" — release-plz is
happy either way.

The moment the PR merges:

1. release-plz creates the tag `v0.2.0` on the merge commit.
2. release-plz pushes the tag.
3. The push triggers `.github/workflows/release.yml`.

### 4. Watch the release workflow

The release workflow takes ~10-15 minutes. Stages:

- **plan** (1 min) — dist computes the manifest.
- **build** (matrix, ~5-8 min) — 6 platforms in parallel.
- **host** (1 min) — uploads artifacts to a draft release.
- **slsa-provenance** (~2 min) — generates `.intoto.jsonl`
  attestations.
- **publish-homebrew-formula** (1 min) — opens PR on the tap repo.
- **publish-npm** (1 min) — publishes to npm.
- **announce** — flips the draft release to published.

If anything in stages 1-3 fails, the GitHub Release stays in draft
and nothing is announced. Fix forward by:

```bash
# Investigate logs, fix the issue on main, then:
git tag -d v0.2.0
git push origin :v0.2.0    # delete the broken tag
# release-plz will re-open the Release PR with the same version.
# Merge it again to retry.
```

For a hotfix without going through release-plz:

```bash
# Bump version manually in Cargo.toml, commit, then:
git tag v0.2.1
git push origin v0.2.1
```

dist won't care that release-plz wasn't involved; the tag is all
it needs.

### 5. Verify

After the release is published, run end-to-end smoke checks. These
also serve as the install instructions in README.md.

**curl installer (Linux/macOS):**

```bash
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/ConductorOne/agent-tui/releases/download/v0.2.0/agent-tui-installer.sh | sh
agent-tui --version
# expected: agent-tui 0.2.0
```

**Homebrew (macOS/Linux):**

```bash
brew tap ConductorOne/baton
brew install agent-tui
agent-tui --version
```

Tap is the existing `ConductorOne/homebrew-baton` — same one used
for baton-* connectors.

**Cargo from source:**

```bash
cargo install agent-tui --version 0.2.0
```

**Container (Public ECR):**

```bash
docker run --rm public.ecr.aws/conductorone/agent-tui:v0.2.0 --version
```

**CDN artifacts (mirror of the GitHub Release):**

```bash
curl -O https://dist.conductorone.com/releases/ConductorOne/agent-tui/v0.2.0/agent-tui-v0.2.0-linux-amd64.tar.gz
```

### 6. Verify the supply chain

Before trusting a downloaded binary on a sensitive system, run:

**Sigstore signature:**

```bash
cosign verify-blob \
  --signature   agent-tui-x86_64-unknown-linux-gnu.tar.gz.sig \
  --certificate agent-tui-x86_64-unknown-linux-gnu.tar.gz.pem \
  --certificate-identity-regexp '^https://github\.com/ConductorOne/agent-tui/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  agent-tui-x86_64-unknown-linux-gnu.tar.gz
```

Expected: `Verified OK`.

**SLSA-3 provenance:**

```bash
slsa-verifier verify-artifact \
  --provenance-path agent-tui-x86_64-unknown-linux-gnu.intoto.jsonl \
  --source-uri      github.com/ConductorOne/agent-tui \
  --source-tag      v0.2.0 \
  agent-tui-x86_64-unknown-linux-gnu.tar.gz
```

Expected: `PASSED: SLSA verification passed`.

**SHA256 checksum:**

```bash
sha256sum -c SHA256SUMS
```

Expected: `agent-tui-x86_64-unknown-linux-gnu.tar.gz: OK` (and one
line per artifact).

## Platform-specific gotchas

### macOS

We don't notarize binaries (yet). First-run prompt on the curl
installer:

```
"agent-tui" cannot be opened because the developer cannot be verified.
```

Workaround (the curl installer prints this; copy into your terminal):

```bash
xattr -d com.apple.quarantine $(which agent-tui)
```

Homebrew users don't see the warning — `brew install` runs through
the trust system automatically. Recommend Homebrew over the curl
installer for macOS until notarization lands (tracked in tracker.md).

### Windows

SmartScreen will show "Windows protected your PC" on first run.
Click "More info" → "Run anyway." This is unavoidable until we
acquire an Authenticode cert (tracked in tracker.md).

The MSI installer (if used) doesn't fix this — unsigned MSI gets
the same warning.

### Linux

No platform-specific signing required. The Sigstore signatures and
SLSA provenance work the same regardless of distro.

## Prereleases

To cut a prerelease (e.g. `v0.2.0-rc.1`) without going through
release-plz:

```bash
# Edit Cargo.toml workspace version to "0.2.0-rc.1".
# Commit + push to main.
git tag v0.2.0-rc.1
git push origin v0.2.0-rc.1
```

dist detects the `-rc.1` suffix and marks the GitHub Release as a
prerelease. Homebrew/npm publish-jobs skip prereleases by default.

To use release-plz for the prerelease (so the changelog stays
right), set the version manually in the Release PR before merging.

## Troubleshooting

### "dist refused: tag doesn't match workspace version"

Cause: you pushed a tag that doesn't match `version = ...` in the
workspace's `Cargo.toml`.

Fix: delete the tag, fix the workspace version, push the matching
tag.

### "Homebrew tap PR didn't open"

Cause: `HOMEBREW_TAP_TOKEN` secret is missing, expired, or
under-scoped.

Fix: regenerate the fine-grained PAT in your GitHub account
settings (contents: write + pull-requests: write on the tap repo),
update the secret.

### "Cross-compile for `<target>` failed"

Cause: a -sys crate doesn't cross-compile via cargo-zigbuild.

Fix:
1. Check the failing step's log for "linker not found" or similar
   — usually means the crate wants a real C toolchain.
2. Either remove the offending dependency, or add an
   `extra-build-setup` step to install the toolchain on that
   target's runner.
3. Common culprit: openssl-sys → use `rustls` features instead.

### "cosign verify-blob: invalid certificate"

Cause: either the certificate-identity-regexp doesn't match, or
the certificate was issued for a different repo / workflow.

Fix: double-check the expected identity matches
`^https://github\.com/ConductorOne/agent-tui/\.github/workflows/release\.yml@refs/tags/v`.

### "slsa-verifier: source-uri mismatch"

Cause: artifact built from a different repo than expected.

Fix: confirm `--source-uri github.com/ConductorOne/agent-tui` matches
the repo the tag was pushed to. If you're verifying a fork, that
fork must run its own release workflow.

## Bootstrapping the pipeline (first-time setup)

If you're standing up the release pipeline from scratch (vs cutting
a release on an existing pipeline), follow `docs/release-rfc.md`'s
phased rollout. Don't try to run this runbook against a repo with
no `release.yml`.

## When to bump the protocol version

`PROTOCOL_VERSION` in `crates/agent-tui-protocol/src/lib.rs` lives
independently of the binary's semver. Bump it when:

- A `Command` variant's wire shape changes incompatibly.
- A `Response` field's meaning changes.
- An error code's numeric value changes.

Adding new commands or new optional fields doesn't bump
PROTOCOL_VERSION — older clients silently ignore unknown fields.

The version bump becomes part of the next regular release; it
doesn't trigger its own release.
