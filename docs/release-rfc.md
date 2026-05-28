---
type: rfc
title: "agent-tui — Release Pipeline (dist + release-plz + sigstore)"
status: draft
author: Paul Querna (with claude-opus)
created: 2026-05-28
harness: claude
---

# RFC: agent-tui — Release Pipeline

- **Status:** Draft v3 (ConductorOne-integrated)
- **Companion to:** `RELEASING.md` (the runbook, written alongside)
- **Trigger:** Time to actually ship the binary. Today there's no
  release infra; we need a "git tag v0.1.0 → 7 platform binaries on
  GitHub Releases with signatures and provenance" pipeline.

## 0. TL;DR

Adopt **dist** (cargo-dist) as the Rust-native build orchestrator,
but **integrate with the existing ConductorOne release
infrastructure** the baton-* connectors use: same S3 bucket
(`connector-artifact-registry`), same CDN
(`dist.conductorone.com`), same Homebrew tap
(`ConductorOne/homebrew-baton`), same Public ECR
(`public.ecr.aws/conductorone/agent-tui`), same Sigstore
attestation conventions (`*.provenance.sigstore.json` +
`*.sbom.sigstore.json`), same Datadog failure notifications. Use
**release-plz** for version bumps. End-to-end:

```
edit code → release-plz PR (bumps version, regenerates changelog)
         → merge PR → tag v0.2.0 (release-plz creates it)
         → release.yml builds 6 Rust targets via dist
         → cosign-sign each artifact (keyless OIDC)
         → syft-SBOM each artifact + cosign-attest as bundle
         → cosign-attest SLSA-1 provenance per artifact
         → upload to S3 (connector-artifact-registry), CDN-served
            via dist.conductorone.com/releases/ConductorOne/agent-tui/<tag>/
         → docker buildx → public.ecr.aws/conductorone/agent-tui:<tag>
         → POST signed manifest.json to dist.conductorone.com/api/v1
         → push formula to ConductorOne/homebrew-baton
         → publish GitHub Release with mirrors of the same artifacts
         → Datadog notify on any failure
```

A `v0.1.0-rc.1` tag does the same but marks the release as a
prerelease and skips Homebrew formula updates.

**What v3 changed from v2:**

- Dropped the plan to create a new `ConductorOne/homebrew-tap` —
  reuse the existing `ConductorOne/homebrew-baton` tap.
- Added `dist.conductorone.com` (S3 + CDN + registry API) as a
  first-class publication target, mirroring the baton-* flow.
- Added Public ECR for container images (replaces ghcr.io plan).
- Added syft for SBOMs + `cosign attest-blob` to wrap them as
  Sigstore bundles — matches the ConductorOne convention used in
  `ConductorOne/github-workflows/release.yaml`.
- Added Datadog failure notification.
- Added AWS OIDC IAM-role prerequisite — RelEng needs to
  provision `GHA-Artifacts-ConductorOne-agent-tui` (and an ECR
  push role) before the workflow can publish.
- Strict semver tag validation aligned with ConductorOne's regex.
- Dropped the `npm` distribution channel from v1 scope —
  not part of the ConductorOne distribution pattern, and `cargo
  install agent-tui` covers the Rust ecosystem.

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
  installer + Homebrew (`ConductorOne/homebrew-baton`) + cargo +
  container (`public.ecr.aws/conductorone/agent-tui`). One source
  of truth (`dist.conductorone.com` + GitHub Release mirror),
  multiple delivery channels.
- **Same release surface as baton-* connectors.** Operators
  managing the ConductorOne stack should be able to find
  agent-tui artifacts using the same patterns they already know
  — same S3 bucket, same CDN, same Homebrew tap, same ECR
  registry, same signature conventions.
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
- **npm wrapper.** Dropped from v3 scope — the ConductorOne
  distribution pattern doesn't use it, and `cargo install
  agent-tui` covers the Rust ecosystem. Can revisit if there's
  demand from Node-shop users.
- **Reusing `ConductorOne/github-workflows/release.yaml`.** That
  workflow is GoReleaser-specific. Rather than fork or extend it
  to cover Rust, we run a parallel Rust pipeline that produces the
  same artifact shapes and pushes to the same downstream
  infrastructure.

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
  --certificate-identity-regexp '^https://github\.com/ConductorOne/agent-tui/' \
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
  --source-uri github.com/ConductorOne/agent-tui \
  --source-tag v0.2.0
```

### 3.5 ConductorOne distribution infrastructure (new in v3)

ConductorOne already runs a release pipeline for ~230 baton-*
connectors. The Rust pipeline reuses every downstream component;
only the build orchestrator differs (dist instead of GoReleaser).

Shared infrastructure we plug into:

| Component | URL / location | Role |
|---|---|---|
| Release reusable workflow (Go) | `ConductorOne/github-workflows/.github/workflows/release.yaml` | Reference for the manifest format, S3 layout, and registry API conventions. We don't call it (it's GoReleaser-only) — we mirror its outputs. |
| S3 bucket | `s3://connector-artifact-registry/releases/<org>/<repo>/<tag>/` (us-west-2) | Canonical artifact storage |
| CDN | `https://dist.conductorone.com/releases/<org>/<repo>/<tag>/` | Public front for the bucket |
| Registry API | `https://dist.conductorone.com/api/v1` | Records release metadata; takes the signed `manifest.json`; OIDC-authenticated with audience `connector-registry` |
| Homebrew tap | `ConductorOne/homebrew-baton` (Formula/ dir, one .rb per artifact) | Public tap; PR'd by the bot account on each release |
| Public ECR | `public.ecr.aws/conductorone/<name>` (us-east-1) | Multi-arch container images, OIDC-pushed |
| AWS IAM (artifacts) | role `GHA-Artifacts-ConductorOne-<repo>` (account 025044153841) | OIDC-assumed by the workflow for S3 + ECR-public writes |
| AWS IAM (Lambda) | role `GitHubActionsECRPushRole-<repo>` (account 168442440833) | Not needed for agent-tui — Lambda is connector-specific |
| Datadog | `us3.datadoghq.com` events API | `notify-release-failure` job posts a structured event on red runs |

The artifact-naming + signing conventions to match exactly:

```
<repo>-v<X.Y.Z>-<os>-<arch>.tar.gz                  (linux: tar.gz, darwin: zip)
<repo>-v<X.Y.Z>-<os>-<arch>.tar.gz.sig              (cosign signature)
<repo>-v<X.Y.Z>-<os>-<arch>.tar.gz.cert             (cosign cert)
<repo>-v<X.Y.Z>-<os>-<arch>.tar.gz.sbom.json        (syft SPDX SBOM)
<repo>-v<X.Y.Z>-<os>-<arch>.tar.gz.sbom.sigstore.json        (SBOM as cosign attestation bundle)
<repo>-v<X.Y.Z>-<os>-<arch>.tar.gz.provenance.sigstore.json  (SLSA-1 provenance bundle)
<repo>_<X.Y.Z>_checksums.txt                        (unified SHA256SUMS — cosign-signed too)
manifest.json                                       (merged manifest of all artifacts — cosign-signed)
manifest.json.sig / .cert / .sigstore.json
```

The `manifest.json` is the keystone — it lists every artifact's
URL + sha256 + size + signature bundle, and is what the registry
API ingests. Schema is implicit in `ConductorOne/github-workflows/
cmd/generate-manifest/`; we'll add a Rust-side equivalent or call
the same Go tool as a step.

**Prerequisites (RelEng must do these once before Phase 4):**

- Create `GHA-Artifacts-ConductorOne-agent-tui` IAM role with the
  same trust policy + S3 + ECR-public permissions as the baton-*
  roles. Naming: see `ConductorOne/github-workflows/scripts/
  derive-iam-role-name.sh` for the exact convention (truncate + hash
  for names >64 chars; ours fits).
- Confirm `homebrew-baton` accepts contributions for non-baton-*
  formulas (no rename needed — the tap is named after baton
  historically but doesn't enforce a baton- prefix; see
  `Formula/baton.rb` for the SDK itself in there).
- Confirm `public.ecr.aws/conductorone/` accepts pushes for
  agent-tui (it should, same OIDC role pattern).
- Confirm `dist.conductorone.com/api/v1` accepts `agent-tui` as a
  non-connector registry record (or — if the API is connector-
  specific — agree on either extending it or skipping registry-API
  recording for agent-tui and treating S3+CDN as the SoT).

### 3.6 Why not GoReleaser

GoReleaser added Rust support in 2024, but as of 2026 its own docs
flag several caveats for Rust: it doesn't install Cargo/Rustup/Zig/
cargo-zigbuild for you, workspace handling has rough edges, and the
ecosystem fit (Cargo.toml metadata, profile.dist) is awkward.
dist is Rust-native and tracks the toolchain.

That said: if we ever want one release pipeline across Go + Rust +
TypeScript projects, GoReleaser is the right call. For agent-tui
specifically, dist wins.

## 4. Repository changes

What lands when this RFC is fully implemented:

```
dist-workspace.toml                     # NEW — dist config (targets, installers, tap)
Cargo.toml                              # MODIFIED — [profile.dist]
.github/workflows/release.yml           # NEW — dist-generated + custom jobs
.github/workflows/release-plz.yml       # NEW — version bump + Release PR
.release-plz.toml                       # NEW — release-plz config
.github/release-extra-jobs/             # NEW — SBOM, cosign, S3, ECR, Datadog steps
   sbom-and-attest.yml
   upload-to-s3.yml
   push-ecr.yml
   notify-datadog.yml
docker/Dockerfile                       # NEW — distroless base, copies musl binary
CHANGELOG.md                            # NEW — managed by release-plz
RELEASING.md                            # NEW — runbook (separate doc)
README.md                               # MODIFIED — install + verify section
docs/release-rfc.md                     # NEW — this doc
```

Cargo.toml repository URL is already correct
(`https://github.com/ConductorOne/agent-tui`) after the rename PR.

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
tap = "ConductorOne/homebrew-tap"

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

Matches ConductorOne's strict semver regex from
`ConductorOne/github-workflows/release.yaml`:

```
^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)
(-((0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*)
(\.(0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*))*))?
(\+([0-9a-zA-Z-]+(\.[0-9a-zA-Z-]+)*))?$
```

In English:

- **Stable:** `v<MAJOR>.<MINOR>.<PATCH>` — `v0.2.0`, `v1.0.0`.
- **Prerelease:** `v<MAJOR>.<MINOR>.<PATCH>-<id>[.<n>]` —
  `v0.2.0-rc.1`, `v1.0.0-alpha.3`. The `dist` workflow detects the
  suffix and marks the GitHub Release as prerelease; ConductorOne
  conventions also skip Homebrew formula updates for prereleases.
- **Build metadata** (`+<build>`) is allowed by the regex but we
  don't use it in practice.

The validate-inputs step in our `release.yml` mirrors this regex
so we reject mismatched tags up front instead of failing midway.

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

- **Registry API agent-tui support.** Does
  `dist.conductorone.com/api/v1` accept a non-connector record?
  v3 default is to skip the registry API call until RelEng
  confirms. The S3 + CDN flow works either way.
- **Public ECR namespace.** Do non-baton-* projects live under
  `public.ecr.aws/conductorone/` or a different prefix? RelEng
  call.
- **Homebrew formula generation.** Does dist's auto-generated
  formula format match what the rest of `homebrew-baton` expects
  (the GoReleaser-generated `# This file was generated by
  GoReleaser. DO NOT EDIT.` header is conventional but not load-
  bearing). If dist's diverges in a way that makes the tap look
  inconsistent, Phase 4b (custom formula generation) is the
  fallback.
- **Apple notarization** — defer until v0.3. baton-* uses gon +
  Apple Developer cert ($99/yr); reuses the same secrets
  (`APPLE_SIGNING_KEY_P12`, `APPLE_SIGNING_KEY_P12_PASSWORD`,
  `AC_PASSWORD`, `AC_PROVIDER`). Adding to agent-tui would be a
  ~half-day workflow change once we decide.
- **Authenticode for Windows MSI** — defer; ConductorOne uses
  GoReleaser Pro for MSI builds, separate cert flow. We can ship
  Windows zips without it; MSI signing waits.
- **crates.io vs binary-only.** release-plz can publish to crates.io
  per crate. For v0.x, do we publish protocol/adapter as separate
  crates, or only the top-level binary? Lean toward binary-only for
  v0.x — the workspace internals are unstable. Bump to publishing
  individual crates at v1.0.
- **Reproducible builds.** Worth doing. Not blocking v0.1 release.

## 10. Phased rollout

Five PRs, in order. Each is independently mergeable.

### Phase 1: Basic dist (1-2 days)

- Fix `repository = "https://github.com/ConductorOne/agent-tui"` in
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

### Phase 4: Homebrew (existing `homebrew-baton` tap) (~half day)

The tap is `ConductorOne/homebrew-baton`. It already publishes
~230 baton-* formulas — see `Formula/baton.rb` for the canonical
shape. Each formula is GoReleaser-generated and points at the
artifact URL on GitHub Releases.

Two sub-options:

- **4a (preferred):** Make `dist`'s built-in homebrew publisher
  target `ConductorOne/homebrew-baton`. Set `tap =
  "ConductorOne/homebrew-baton"` in `dist-workspace.toml` + a
  `HOMEBREW_TAP_TOKEN` (fine-grained PAT, contents: write,
  pull-requests: write, scoped to homebrew-baton only). On first
  run, dist opens a PR adding `Formula/agent-tui.rb`. Subsequent
  runs update it.
- **4b:** Write a small workflow step (post-dist) that generates a
  Ruby formula from the `manifest.json` we already produce — same
  shape as the baton formulas — and opens a PR against
  `homebrew-baton/Formula/agent-tui.rb`. ~50 LOC of bash + jq + an
  ERB template. Use this if dist's generated formula diverges from
  the baton convention in ways that bother RelEng.

**Prereqs from SRE/RelEng:**
- Create a fine-grained PAT and save as `HOMEBREW_TAP_TOKEN` secret
  on the agent-tui repo. PAT must have write access to
  `ConductorOne/homebrew-baton` only.

**Exit criteria:** `brew install ConductorOne/baton/agent-tui`
installs the binary and `agent-tui --version` matches the tagged
version.

### Phase 5: dist.conductorone.com S3 + CDN + registry API (~2 days)

This is the meat of the ConductorOne integration. Mirrors the
artifact shape, signing conventions, and S3 layout that baton-*
connectors already publish.

**Prereqs from SRE/RelEng (must precede):**
- Provision IAM role `GHA-Artifacts-ConductorOne-agent-tui` (account
  `025044153841`) with S3 write access to
  `connector-artifact-registry/releases/ConductorOne/agent-tui/*`
  and OIDC trust for the agent-tui repo's release workflow. Same
  trust policy as baton-* roles use; naming convention from
  `ConductorOne/github-workflows/scripts/derive-iam-role-name.sh`.
- Confirm whether `dist.conductorone.com/api/v1` accepts a
  non-connector record. If not, agree on either (a) extending the
  API to accept agent-tui or (b) skipping the registry-API record
  and treating S3+CDN as SoT. v3 defaults to (b) until RelEng
  confirms (a).

**Workflow steps to add (as dist `extra-jobs` or post-build hooks):**

1. **SBOM via syft.** For each archive: `syft <archive> -o spdx-json
   > <archive>.sbom.json`. Match the ConductorOne naming exactly.
2. **Sign SBOMs as attestations.** `cosign attest-blob --yes
   --predicate <archive>.sbom.json --type https://spdx.dev/Document
   --bundle <archive>.sbom.sigstore.json <archive>`. Bundle goes
   to S3.
3. **SLSA-1 provenance.** Generate a predicate JSON using the
   template from
   `ConductorOne/github-workflows/templates/.slsa-provenance-
   predicate-template.json.tmpl`, substituting our repo +
   workflow-ref. Then `cosign attest-blob --type slsaprovenance1
   --bundle <archive>.provenance.sigstore.json <archive>`.
4. **Sign individual archives.** `cosign sign-blob --yes
   --output-signature <archive>.sig --output-certificate
   <archive>.cert <archive>`.
5. **Unified checksums.** Merge per-platform SHA256SUMS into
   `agent-tui_<X.Y.Z>_checksums.txt`, then cosign-sign + attest.
6. **Generate manifest.json.** Either call the Go tool from
   `ConductorOne/github-workflows/cmd/generate-manifest/` (cleanest
   — same output as the baton-* pipeline) or write a Rust port.
   Then cosign-sign + attest the manifest.
7. **Upload to S3.** `aws s3 cp dist/* s3://connector-artifact-
   registry/releases/ConductorOne/agent-tui/<tag>/ --cache-control
   "public,max-age=31536000,immutable"` for each artifact. AWS
   credentials via `aws-actions/configure-aws-credentials@v5` using
   the OIDC role.
8. **(Conditional) Call registry API.** With audience-scoped OIDC
   token, POST the signed `manifest.json` to
   `dist.conductorone.com/api/v1`. Same Go tool from
   `ConductorOne/github-workflows/cmd/record-release/`.

**Verification step:** mirror
`ConductorOne/github-workflows/scripts/validate-release-artifacts.sh`
— `cosign verify-blob` on each archive + checksums + manifest.

**Exit criteria:**
- Artifacts at `https://dist.conductorone.com/releases/ConductorOne/
  agent-tui/v0.2.0/<file>` resolve.
- `cosign verify-blob` succeeds for each archive against
  `--certificate-identity-regexp '^https://github\.com/ConductorOne/
  agent-tui/\.github/workflows/release\.yml@refs/tags/v'`.
- Signed `manifest.json` resolves and verifies.
- (If 3.5-(a) is chosen) registry API record exists for the tag.

### Phase 6: Container image to Public ECR (~1 day)

Mirrors the `goreleaser-docker` job in
`ConductorOne/github-workflows/release.yaml`.

**Image:** `public.ecr.aws/conductorone/agent-tui:<tag>` +
`public.ecr.aws/conductorone/agent-tui:latest`. Multi-arch
(linux/amd64 + linux/arm64).

**Steps:**

- Multi-stage Dockerfile using the musl-static binary the dist
  matrix already produces (we get this for free since musl is in
  our targets list). Final stage:
  `gcr.io/distroless/static-debian11:nonroot` (same base the
  baton-* connectors use). Single `ENTRYPOINT ["/agent-tui"]`.
- `docker buildx build --platform linux/amd64,linux/arm64 --push`
  using OIDC-assumed `GHA-Artifacts-ConductorOne-agent-tui`.
- `cosign attest --yes --type https://slsa.dev/provenance/v1
  --predicate <predicate.json> <image>@<digest>` for SLSA
  attestation on the image (separate from the blob attestations).
- Add the image to the merged `manifest.json` so consumers see
  both the binary and the container locations.

**Open question:** ConductorOne's pattern is to push to the
artifacts AWS account (025044153841) for ECR-public. Confirm with
RelEng that agent-tui (non-baton) lives in the same registry
namespace — `public.ecr.aws/conductorone/agent-tui` — or whether
it should be under a different prefix.

**Exit criteria:**
- `docker pull public.ecr.aws/conductorone/agent-tui:v0.2.0` works
  from a clean host.
- `cosign verify public.ecr.aws/conductorone/agent-tui:v0.2.0`
  succeeds against our identity.

### Phase 7: Datadog failure notification (~half day)

Mirror the `notify-release-failure` job from
`ConductorOne/github-workflows/release.yaml`. Posts a Datadog event
to `us3.datadoghq.com/api/v1/events` on any red job in the release
pipeline. Useful for the SRE on-call rotation to see release-flow
regressions across baton-* and agent-tui in the same dashboard.

**Prereqs:**
- Add `DATADOG_API_KEY` to the agent-tui repo secrets (same key the
  baton-* repos already share, scoped through GitHub org secrets).

**Exit criteria:** force a fake failure on a test tag; confirm the
event lands in Datadog with `github_repository:ConductorOne/agent-
tui`.

Total across all phases: **7-10 days** of work spread over 7 PRs.
The bulk is Phase 5 (dist.conductorone.com integration); each
other phase is small but AWS/IAM prereq coordination with SRE can
add wall-clock delay independent of engineering time.

## 11. Test plan

### Pre-merge tests (small unit-style)

- `dist plan` runs locally without error after Phase 1 lands.
- `release.yml` is regenerable via `dist init` and the diff is
  small + bounded to the extra-jobs hooks we control.
- `release-plz` opens a Release PR within ~minutes of a push to
  main (tested by pushing a no-op commit).
- The `derive-iam-role-name.sh` script (from
  `ConductorOne/github-workflows`) returns the expected role names
  when invoked with our `--suffix ConductorOne-agent-tui`.

### Tag-driven tests (each phase has at least one)

- **Phase 1:** `v0.1.0-rc.0` validates the dist matrix builds 6
  platforms, lands on GitHub Releases. Delete the prerelease + tag
  after smoke.
- **Phase 2:** `v0.1.0-rc.1` validates cosign signs each artifact.
- **Phase 3:** `v0.1.0-rc.2` validates release-plz → tag → release
  chain (or the first stable `v0.1.0` if Phase 3 lands after
  release-plz is already wired).
- **Phase 4:** `v0.1.0` is the first release with a Homebrew
  formula opened against `ConductorOne/homebrew-baton`.
- **Phase 5:** `v0.2.0-rc.0` validates S3 upload + manifest + (if
  applicable) registry API recording.
- **Phase 6:** same `v0.2.0-rc.0` validates Public ECR push +
  cosign attest.
- **Phase 7:** force a failure on a test branch + tag; confirm
  Datadog gets the event.
- `v0.2.0` is the first complete release with everything wired.

### Post-publish smoke (every release)

- Clean macOS arm64 / amd64 + Linux x86_64 / aarch64 + Windows VM:
  install via curl installer, `agent-tui --version` matches.
- `brew install ConductorOne/baton/agent-tui` on macOS — version
  matches.
- `docker run --rm public.ecr.aws/conductorone/agent-tui:vX.Y.Z
  --version` — version matches.
- `cosign verify-blob` succeeds on every archive with the expected
  identity regex.
- `cosign verify` succeeds on the container image.
- The signed `manifest.json` at
  `https://dist.conductorone.com/releases/ConductorOne/agent-tui/<tag>/manifest.json`
  resolves + verifies.
- (Optional, depends on §10 resolution) `dist.conductorone.com/api/v1`
  shows a record for the tag.

## 12. References

- `dist` (cargo-dist) — github.com/axodotdev/cargo-dist
- `release-plz` — github.com/release-plz/release-plz
- Sigstore cosign — github.com/sigstore/cosign
- SLSA generator — github.com/slsa-framework/slsa-github-generator
- The `RELEASING.md` runbook lives alongside this RFC.
