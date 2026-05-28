---
type: rfc
title: "agent-tui — Release Pipeline (dist + release-plz + sigstore)"
status: draft
author: Paul Querna (with claude-opus)
created: 2026-05-28
harness: claude
---

# RFC: agent-tui — Release Pipeline

- **Status:** Draft v2
- **Companion to:** `RELEASING.md` (the runbook, written alongside)
- **Trigger:** Time to actually ship the binary. Today there's no
  release infra; we need a "git tag v0.1.0 → 7 platform binaries on
  GitHub Releases with signatures and provenance" pipeline.

## 0. TL;DR

Adopt **dist** (formerly `cargo-dist`) for the binary build /
package / publish pipeline, and **release-plz** for the
version-bump-and-tag step. Layer **Sigstore keyless signing** and
**SLSA-3 provenance** on top. The whole thing is driven by tags
matching `v<semver>` on the default branch:

```
edit code → release-plz PR (bumps version, regenerates changelog)
         → merge PR
         → tag v0.2.0 (release-plz can do this too)
         → dist workflow builds 7 platforms, signs, attests, publishes
         → GitHub Release + Homebrew tap + npm + curl installer
```

A `v0.1.0-rc.1` tag does the same thing but marks the release as a
prerelease and skips the "publish to stable channels" steps.

## 1. Goals

- **One git tag triggers everything.** No manual artifact uploads.
- **Cryptographically verifiable.** Every published binary has
  Sigstore signatures + SLSA-3 provenance + SHA256SUMS, all in the
  same GitHub Release.
- **Reproducible inputs.** Locked dependencies, pinned Rust toolchain,
  pinned dist version. We don't go for byte-reproducible output in v1
  (rustc embeds paths), but the inputs are pinned.
- **No private keys to manage.** Keyless signing via GitHub Actions
  OIDC. No GPG keys, no Apple cert (for v1), no Authenticode cert.
- **Install via the user's package manager of choice.** curl
  installer + Homebrew + npm + cargo + container. One source of
  truth (the GitHub Release), multiple delivery channels.
- **Honest about what's signed and how.** The README and
  `RELEASING.md` document the verification steps so users can
  confirm what they're installing.

## 2. Non-goals (for v1)

- **Apple notarization.** Defer until v0.3 — needs an Apple Developer
  account ($99/yr). What users see without it: a macOS Gatekeeper
  dialog ("agent-tui cannot be opened because the developer cannot
  be verified") the first time they run the binary. Workaround
  documented in the installer output and `RELEASING.md`:
  `xattr -d com.apple.quarantine $(which agent-tui)`. Users
  installing via Homebrew bypass Gatekeeper entirely; users running
  the curl installer are the ones who see the warning.
- **Authenticode for Windows.** Defer; cert is ~$200/yr (EV cert is
  more). Without it, SmartScreen will show a "Windows protected your
  PC" warning on first run; users click "More info → Run anyway."
  An MSI installer with `dist`'s built-in MSI generation does NOT
  fix this — unsigned MSI triggers the same warning. No workaround
  beyond clicking through, or buying a cert.
- **Linux package managers** (.deb, .rpm, AUR, AppImage). cargo-deb /
  cargo-generate-rpm are easy to add later; not required for v1.
- **Reproducible builds.** Stretch goal. The build is *deterministic
  enough* (locked deps, pinned toolchain) but rustc still embeds
  per-build paths in debug info.
- **Multi-binary releases.** agent-tui ships one binary. The
  workspace metadata won't try to disambiguate.
- **Continuous deployment.** Releases are deliberate, tag-driven.
  Nothing auto-ships on `main`.

## 3. Tool stack

### 3.1 `dist` (formerly cargo-dist)

Project repo: `github.com/axodotdev/cargo-dist`. Binary name on the
PATH is `dist`. Confirmed locally at v0.28.0 (current).

What dist does:

- Reads `[dist]` config from `dist-workspace.toml` (NEW format in
  0.28+; replaces `[workspace.metadata.dist]`).
- Generates `.github/workflows/release.yml` (commit it; regenerate
  with `dist init`).
- On a tag matching `**[0-9]+.[0-9]+.[0-9]+*`, the workflow:
  1. **plan**: runs `dist plan` to produce `dist-manifest.json`
     describing every artifact + checksum + signature path.
  2. **build (matrix)**: per-target jobs that build the binary,
     tar/zip-archive it, generate `SHA256SUMS`.
  3. **host**: uploads artifacts to a draft GitHub Release.
  4. **publish-homebrew-formula** / **publish-npm**: opens PRs /
     publishes to downstream channels.
  5. **announce**: flips the release from draft to published.

Why dist beats hand-rolled YAML:

- Cross-compilation via `cargo-zigbuild` (Linux) and `cargo-xwin`
  (Windows) is wired in — no manual cross-toolchains to maintain.
- The generated workflow handles tag parsing, prerelease detection,
  partial-failure recovery, and `--allow-dirty` semantics correctly.
- Installer scripts (`install.sh`, `install.ps1`) are generated and
  hosted from the GitHub Release — `curl … | sh` Just Works.
- Homebrew formula and npm wrapper are auto-generated from the same
  manifest. Single source of truth.

### 3.2 `release-plz`

Project repo: `github.com/release-plz/release-plz`.

What release-plz does:

- Watches `main`. When commits land, opens a "Release PR" that bumps
  the workspace version, regenerates `CHANGELOG.md` from conventional
  commits, and updates Cargo.lock.
- On merge of the Release PR, **creates the git tag and (if
  configured) publishes to crates.io**.
- Tag push triggers the `dist` workflow above.

Why release-plz beats hand-bumped versions:

- Conventional-commit-driven changelogs (we already mostly write
  conventional commits — `feat(…)`, `fix(…)`).
- Cargo.toml + Cargo.lock version updates stay in sync.
- The Release PR is reviewable: human checks the proposed version
  bump + changelog before the tag exists.
- crates.io publishing happens after merge, not on tag push, so a bad
  tag doesn't poison crates.io.

### 3.3 Sigstore / cosign — keyless signing

Project: `sigstore/cosign`.

What we sign:

- Every tarball / zip in the GitHub Release gets a
  `.tar.gz.sig` + `.tar.gz.pem` pair.
- The `SHA256SUMS` file gets the same.
- The container image (separate workflow) gets its manifest signed.

How: `cosign sign-blob --yes --output-signature <f>.sig
--output-certificate <f>.pem <f>` with the GitHub Actions OIDC token
in scope. No key material to store. Sigstore's Rekor log keeps a
public transparency record.

User verification:

```bash
cosign verify-blob agent-tui-x86_64-linux.tar.gz \
  --signature agent-tui-x86_64-linux.tar.gz.sig \
  --certificate agent-tui-x86_64-linux.tar.gz.pem \
  --certificate-identity-regexp '^https://github\.com/ductone/agent-tui/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

### 3.4 SLSA-3 provenance

Project: `slsa-framework/slsa-github-generator`.

What we attest: every artifact's `dist-manifest.json` entry is bound
to the source commit + workflow that produced it. The provenance is
itself signed via Sigstore.

How: add the reusable workflow `slsa-github-generator/.github/
workflows/generator_generic_slsa3.yml@v2.0.0` as a dependent job in
release.yml. It consumes the dist artifacts, produces
`*.intoto.jsonl` provenance files, attaches them to the release.

User verification:

```bash
slsa-verifier verify-artifact agent-tui-x86_64-linux.tar.gz \
  --provenance-path agent-tui-x86_64-linux.intoto.jsonl \
  --source-uri github.com/ductone/agent-tui \
  --source-tag v0.2.0
```

### 3.5 Why not GoReleaser

GoReleaser added Rust support in 2024, but as of 2026 its own docs
flag several caveats for Rust: it doesn't install Cargo/Rustup/Zig/
cargo-zigbuild for you, workspace handling has rough edges, and the
ecosystem fit (Cargo.toml metadata, profile.dist) is awkward.
dist is Rust-native and tracks the toolchain.

That said: if we ever want one release pipeline across Go + Rust +
TypeScript projects, GoReleaser is the right call. For agent-tui
specifically, dist wins.

## 4. Repository changes

What lands when this RFC is implemented:

```
dist-workspace.toml                    # NEW — dist config
Cargo.toml                              # MODIFIED — [profile.dist] + repository URL
.github/workflows/release.yml           # NEW — generated by `dist init`
.github/workflows/release-plz.yml       # NEW — release-plz action
.github/workflows/slsa.yml              # NEW — SLSA-3 reusable workflow caller
.release-plz.toml                       # NEW — release-plz config
CHANGELOG.md                            # NEW — managed by release-plz
RELEASING.md                            # NEW — runbook (separate doc)
README.md                               # MODIFIED — install + verify section
docs/release-rfc.md                     # NEW — this doc
```

Cargo.toml diff (workspace `[package]` section):

```diff
 [workspace.package]
 version = "0.1.0"
 license = "Apache-2.0"
-repository = "https://github.com/agent-tui/agent-tui"
+repository = "https://github.com/ductone/agent-tui"
 authors = ["agent-tui contributors"]
```

(The current `agent-tui/agent-tui` URL is wrong; dist refuses to
init until this is set to the real path.)

`[profile.dist]` block dist injects into Cargo.toml:

```toml
[profile.dist]
inherits = "release"
lto = "thin"
```

We may want `lto = "fat"` and `codegen-units = 1` for smaller
binaries. ~10% size win vs thin LTO; ~5x build time. Defer the
decision; thin LTO is dist's default and is fine for v1.

## 5. dist-workspace.toml (the config)

Generated by `dist init`; we'll commit something like:

```toml
[workspace]
members = ["cargo:."]

[dist]
# Pin dist version so the workflow is reproducible.
cargo-dist-version = "0.28.0"

# CI backends to support.
ci = "github"

# Installers to generate.
installers = ["shell", "powershell", "homebrew", "npm"]

# Target platforms. Same set we already cross-check in xtask.
targets = [
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
]

# Publish-jobs configure downstream channels.
publish-jobs = ["homebrew", "npm"]

# Where the homebrew tap lives. Must be a separate repo we control.
tap = "ductone/homebrew-tap"

# When dist runs as part of a pull_request, run in `plan` mode only —
# don't try to publish anything from a draft.
pr-run-mode = "plan"

# GitHub Actions runner overrides. Keep ubuntu-latest = 24.04 since
# our integration jobs already de-AppArmor for bwrap there.
github-build-setup = "../hooks/dist-setup.yml"

# Generate Sigstore attestations on every artifact.
github-attestations = true
```

Two pieces above are worth flagging:

- **`github-attestations`** — when true, dist enables GitHub's
  native build provenance (the same one `gh attestation verify`
  uses). dist 0.28's exact flag name needs verification against
  the project's reference config — micro-test step in Phase 2 will
  catch any mismatch. If the flag isn't named that, we drop in a
  manual cosign step instead; see §7.1.
- **`github-build-setup`** — points at a workflow fragment we
  maintain (e.g. `.github/release-build-setup.yml`) so we can
  inject release-time pre-steps (system deps, secret env vars)
  without losing them on every `dist init` regeneration. Used to
  forward `HOMEBREW_TAP_TOKEN` and any future Apple/Windows signing
  secrets.

## 6. Versioning + tagging convention

### 6.1 Version policy

- `0.x.y` until v1.0. Breaking changes can land in any minor bump.
- Once `1.0.0` ships, semver semantics:
  - `MAJOR` for protocol-incompatible changes (CLI ↔ daemon).
  - `MINOR` for additive surface (new commands, new selectors).
  - `PATCH` for bug-fix-only releases.
- The `PROTOCOL_VERSION` constant in `agent-tui-protocol` advances
  on its own cadence; it's bumped on incompatible wire changes
  regardless of the binary's semver.

### 6.2 Tag format

- Stable: `v<MAJOR>.<MINOR>.<PATCH>` — `v0.2.0`, `v1.0.0`.
- Prerelease: `v<MAJOR>.<MINOR>.<PATCH>-<kind>.<n>` —
  `v0.2.0-rc.1`, `v1.0.0-alpha.3`. dist detects the suffix and marks
  the GitHub Release as prerelease.
- The matching glob in `release.yml` is `**[0-9]+.[0-9]+.[0-9]+*`.
  (dist's default; covers `v0.1.0` and `0.1.0` and `releases/0.1.0`.)
  We will only use the `v`-prefix form; the others work for
  compatibility.

### 6.3 What triggers what

| Action | Effect |
|---|---|
| Commit to a feature branch | nothing |
| Merge PR to `main` | release-plz updates the Release PR (rebases, regenerates changelog) |
| Merge release-plz's Release PR | release-plz creates tag `v<NEW>` and pushes |
| Tag push `v0.2.0` | dist workflow builds + signs + publishes |
| Tag push `v0.2.0-rc.1` | dist workflow builds + signs, marks as prerelease, skips homebrew/npm |
| Manual tag (`git tag`) | dist workflow runs the same as above. release-plz is bypassed; CHANGELOG.md must be hand-edited. |

The "manual tag" path is the escape hatch for hotfixes.

## 7. GitHub Actions structure

Five workflow files after this lands. Each has a single
responsibility:

```
.github/workflows/
├── ci.yml              (existing) — fmt, clippy, test, integration
├── release.yml         (dist-generated) — tag push → artifacts
├── release-plz.yml     — push to main → Release PR maintenance
├── slsa.yml            — release.yml triggers slsa.yml on artifact upload
└── docker.yml          (later) — same tag → container image
```

`release.yml` and `slsa.yml` form a chain: dist publishes artifacts,
slsa.yml uses `workflow_run` triggered on release.yml's completion
to attest them. This is the SLSA project's recommended split.

### 7.1 release.yml — what dist generates

Roughly 290 lines of YAML, four jobs:

```
plan ──┬─ build (matrix of targets) ──┬─ host (upload to draft) ──┬─ publish-homebrew-formula
       │                              │                            ├─ publish-npm
       └─ source-tarball              │                            └─ announce (flip release to published)
                                       └─ generate-attestations
```

We don't author this file by hand. Regenerate via `dist init --no-config`
when bumping dist version.

### 7.2 release-plz.yml — written manually

Standard `release-plz/action@v0.5`:

```yaml
name: Release-plz
on:
  push:
    branches: [main]

jobs:
  release-plz:
    name: Release-plz
    runs-on: ubuntu-latest
    permissions:
      pull-requests: write
      contents: write
    steps:
      - uses: actions/checkout@v4
        with: { fetch-depth: 0 }
      - uses: dtolnay/rust-toolchain@stable
      - uses: MarcoIeni/release-plz-action@v0.5
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          # CARGO_REGISTRY_TOKEN set only when we want crates.io
          # publishing on. For v0.1.x, set --publish-on=git-tag for
          # binary releases without crates.io.
```

Companion `.release-plz.toml`:

```toml
[workspace]
# Don't publish to crates.io yet — workspace internals are unstable.
# This makes release-plz emit a tag + GitHub release but stop short
# of `cargo publish`. We'll flip this to true at v1.0.
publish = false

# release-plz writes the changelog from conventional-commit prefixes
# (`feat(scope): …`, `fix(scope): …`, `BREAKING CHANGE: …`).
changelog_update = true

# Skip individual sub-crates; only the top-level agent-tui binary
# crate produces a public release.
[[package]]
name = "agent-tui"
publish = false   # → flip to true at v1.0 if we want crates.io

[[package]]
name = "agent-tui-protocol"
release = false   # → not released as its own artifact

[[package]]
name = "agent-tui-daemon"
release = false

# Same for the other workspace members …
```

The `release = false` on inner crates tells release-plz "this is
not its own release vehicle." Without those entries, release-plz
would try to cut releases for every crate in the workspace.

### 7.3 SLSA-3 provenance — downstream job in `release.yml`

**Original v1 design** was a separate `slsa.yml` triggered via
`workflow_run`. That's the SLSA framework's *legacy* recommendation
and has a real problem: `workflow_run`-triggered runs lose the
permissions/secrets of the upstream workflow, and the `id-token`
scope needed for keyless signing doesn't propagate. SLSA's current
guidance is to call the reusable workflow as a **downstream job in
the same workflow** as the build.

So we patch `release.yml` via `dist`'s `extra-jobs` hook
(supported in 0.28+) to add the SLSA job after the dist matrix
finishes:

```yaml
# .github/release-extra-jobs.yml  — included by dist's release.yml
slsa-provenance:
  needs: [build-local-artifacts, host]
  permissions:
    actions: read
    id-token: write
    contents: write
  uses: slsa-framework/slsa-github-generator/.github/workflows/generator_generic_slsa3.yml@v2.0.0
  with:
    base64-subjects: ${{ needs.build-local-artifacts.outputs.subjects }}
    upload-assets: true
    upload-tag-name: ${{ needs.plan.outputs.tag }}
```

The `subjects` output is a base64-encoded list of
`<sha256>  <filename>` pairs covering every dist artifact. dist
0.28 emits this from `build-local-artifacts` automatically when
the SLSA hook is wired in.

If `extra-jobs` isn't available we fall back to a hand-maintained
`release.yml` patch (regenerable diff), but this should land
upstream — file an issue against dist if it doesn't.

## 8. The release runbook (summary)

The full version is `RELEASING.md`. Summary here:

1. **Land features on `main`.** Conventional commits help release-plz
   write good changelogs (`feat(selector): …`, `fix(pty): …`).
2. **Wait for the Release PR.** release-plz keeps it up to date.
   Review the proposed version + changelog.
3. **Merge the Release PR.** release-plz tags + pushes.
4. **Watch the Release workflow.** ~10-15 min. If it goes red, the
   tag is still there but no release was published. Investigate,
   fix forward, push a new tag (`v0.2.1`).
5. **Verify post-publish.** Install via curl on a clean machine,
   confirm `agent-tui --version` matches, run `cosign verify-blob`
   and `slsa-verifier verify-artifact` against the published
   artifacts.
6. **Announce.** GitHub Release page is the canonical announcement;
   release-plz's changelog is the canonical changelog.

## 9. Open questions

- **Tap repo bootstrapping.** `ductone/homebrew-tap` doesn't exist
  yet. We need to create it before dist's first run, or the
  publish-homebrew-formula job will error. Quick fix — initialize an
  empty repo with a `Formula/` directory.
- **npm package name.** `@ductone/agent-tui` requires the `ductone`
  npm org. Alternative: ship as `agent-tui` (unscoped, hope it's
  free). Need to check; defer until step 4 of the phased rollout.
- **crates.io vs binary-only.** release-plz can publish to crates.io
  per crate. For v0.x, do we publish protocol/adapter as separate
  crates, or only the top-level binary? Lean toward binary-only for
  v0.x — the workspace internals are unstable. Bump to publishing
  individual crates at v1.0.
- **Container image.** Defer to a separate RFC. Plumbing
  is non-trivial (multi-arch buildx, sign with cosign). The musl
  binary inside the container is what we already build in the dist
  matrix; the Dockerfile is a thin wrapper.
- **Reproducible builds.** Worth doing. Not blocking v0.1 release.

## 10. Phased rollout

Five PRs, in order. Each is independently mergeable.

### Phase 1: Basic dist (1-2 days)

- Fix `repository = "https://github.com/ductone/agent-tui"` in
  `[workspace.package]`.
- Bump workspace version to `0.1.0-rc.0` (or whatever prerelease
  suffix we want). dist refuses to tag at a version that doesn't
  match the workspace.
- `dist init --yes` to generate `dist-workspace.toml` +
  `[profile.dist]` + `release.yml`. The first run prompts for
  installer choices interactively unless flags are passed.
- Push tag `v0.1.0-rc.0` to validate the matrix builds. **Don't
  reuse a "test" tag like `v0.0.0-rc.0`** — dist will reject it
  because the workspace says `0.1.0`. The prerelease suffix marks
  the GitHub Release as a prerelease so users don't auto-pull it.
- Confirm 6 binaries land on the GitHub Release.
- If broken: investigate, push `v0.1.0-rc.1`, repeat. Tags are
  cheap.

**Real cost driver:** the cross-compile matrix typically breaks
on first run — usually openssl-sys or some other -sys crate that
doesn't cross-cleanly. cargo-zigbuild handles most cases. Allocate
2 days, not 1.

**Exit criteria:** `dist plan` succeeds locally; release workflow
publishes all 6 platforms from `v0.1.0-rc.0`.

### Phase 2: Sigstore signing (half a day)

- Set the appropriate dist attestation flag (`github-attestations`
  or `dist init`-suggested equivalent).
- Add a downstream `cosign sign-blob` step via `extra-jobs` hook for
  belt-and-suspenders signatures (Sigstore Rekor is more widely
  verified than GitHub attestations alone).
- Push `v0.1.0-rc.1`. Verify the `.sig` + `.pem` show up.
- Document verification in `RELEASING.md` with copy-pasteable
  `cosign verify-blob` invocation.

**Exit criteria:** `cosign verify-blob` succeeds against the
published artifacts; the message specifies the expected
`--certificate-identity-regexp` and `--certificate-oidc-issuer`.

### Phase 3: release-plz (half a day)

- Add `.github/workflows/release-plz.yml`.
- Add `.release-plz.toml` (sketched in §7.2).
- Push to main; confirm a Release PR appears within ~minutes.
- Merge it; confirm the tag is created and release.yml fires.

**Exit criteria:** end-to-end "merge to main → Release PR → merge
→ tag → release" works without manual steps. The first Release PR
should propose bumping `0.1.0-rc.1` → `0.1.0` (since rc-N → stable
is what release-plz infers from "no unreleased changes").

### Phase 4: Distribution channels (1-2 days)

**Bootstrap step (must precede first publish):**

- Create `ductone/homebrew-tap` repo, empty except for a placeholder
  `Formula/.gitkeep`.
- Generate a fine-grained PAT scoped to that tap repo
  (`contents: write`, `pull-requests: write`). Store as
  `HOMEBREW_TAP_TOKEN` in the agent-tui repo's secrets. Wire it
  into release.yml via `github-build-setup`.
- Decide on npm name: `@ductone/agent-tui` (scoped, requires `ductone`
  npm org) vs `agent-tui` (unscoped, must be available). Check
  availability first. If npm org doesn't exist, create it; npm
  scopes are free.
- Set `NPM_TOKEN` secret if publishing to npm.

**Wire it up:**

- Add `publish-jobs = ["homebrew", "npm"]` to dist config.
- Set `tap = "ductone/homebrew-tap"`.
- Push `v0.2.0` for the first real distribution-channel release.

**Exit criteria:** `brew install ductone/tap/agent-tui`,
`npm install -g @ductone/agent-tui` (or unscoped equivalent), and
the curl installer all install the same binary verifiable to the
same Sigstore cert.

### Phase 5: SLSA-3 provenance (half a day to one day)

- Add the SLSA generator as a downstream job in release.yml via
  `extra-jobs` (see §7.3 — original `workflow_run` approach is
  discarded as buggy).
- Push `v0.2.1`. Verify `*.intoto.jsonl` attestations land on the
  release.
- Document `slsa-verifier verify-artifact` usage in `RELEASING.md`.

**Exit criteria:** `slsa-verifier` succeeds against the published
artifacts.

Total: **5-7 days** spread over 5 PRs. The original "3 days"
estimate was optimistic — Phase 1 cross-compile debugging is the
common time sink.

## 11. Test plan

### Pre-merge tests (small unit-style)

- `dist plan` runs locally without error after Phase 1 lands.
- `release.yml` is regenerable via `dist init` and the diff is empty.
- `release-plz` opens a Release PR within ~30s of a push to main
  (tested by pushing a no-op commit).

### Tag-driven tests (each phase has at least one)

- `v0.0.0-rc.N` tags for Phases 1-3 validate the pipeline without
  publishing real artifacts (mark as prerelease, delete after).
- `v0.1.0-rc.0` is the first "real" prerelease — published, but
  marked as prerelease so users don't auto-pull it.
- `v0.1.0` is the first stable release after all phases land.

### Post-publish smoke

- Clean macOS VM: `curl --proto '=https' --tlsv1.2 -LsSf
  https://github.com/ductone/agent-tui/releases/download/v0.1.0/agent-tui-installer.sh
  | sh`, then `agent-tui --version`.
- Same on Linux x86_64 + aarch64 + Windows.
- `cosign verify-blob` on each artifact.
- `slsa-verifier verify-artifact` on each artifact.

## 12. References

- `dist` (cargo-dist) — github.com/axodotdev/cargo-dist
- `release-plz` — github.com/release-plz/release-plz
- Sigstore cosign — github.com/sigstore/cosign
- SLSA generator — github.com/slsa-framework/slsa-github-generator
- The `RELEASING.md` runbook lives alongside this RFC.
