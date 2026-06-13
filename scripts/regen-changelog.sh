#!/usr/bin/env bash
#
# Deterministic regeneration of crates/agent-tui/CHANGELOG.md.
#
# WHY THIS EXISTS (not just `git cliff -o`):
#   git-cliff attributes each commit to a release by walking the full history in
#   a single pass and bucketing commits under the nearest tag. Two properties of
#   this repo's history break that single-pass walk:
#
#     1. Release PRs that were MERGE-COMMITTED put the `chore: release vX.Y.Z`
#        commit (the commit each tag points at) on a later COMMIT DATE than the
#        feature commits it ships. git-cliff's date-ordered walk then files
#        those feature commits under the PREVIOUS tag (off-by-one attribution).
#     2. commit_parsers skip the tag-pointed `chore: release` commit. A version
#        whose ONLY own commit is that skipped release commit (0.1.12, 0.1.10,
#        0.1.7, 0.1.2 here) ends up empty, so git-cliff drops its section header
#        entirely (missing-version defect — including the latest shipped tag).
#
#   The durable repo-level fix is to SQUASH-merge release-plz PRs (see the
#   RECURRENCE note in .release-plz.toml). But history that already happened
#   can't be rewritten, so this file is regenerated deterministically: each tag
#   is rendered from its OWN explicit `vPREV..vTAG` range with `--tag vTAG`,
#   which pins every commit to the correct release and physically cannot drift.
#
# The git-cliff config below is the standalone-tool equivalent of the
# [changelog] block release-plz drives in .release-plz.toml — same header/body,
# same commit_parsers / preprocessors. (git-cliff reads commit_parsers etc. from
# a [git] table; release-plz re-maps them from its own [changelog] table, so the
# two files express the same intent in each tool's native schema.)
#
# Re-run after release history changes (rare — this is static history):
#   GIT_CLIFF=/path/to/git-cliff scripts/regen-changelog.sh
# git-cliff must be 2.13.x (the git-cliff-core version release-plz 0.3.159 bundles).
#
set -euo pipefail

REPO="$(git rev-parse --show-toplevel)"
cd "$REPO"

GIT_CLIFF="${GIT_CLIFF:-git-cliff}"
OUT="$REPO/crates/agent-tui/CHANGELOG.md"
# git-cliff requires the config file to carry a recognized extension (.toml),
# otherwise it errors with "not of a supported file format".
CFG="$(mktemp --suffix=.toml)"
trap 'rm -f "$CFG"' EXIT

cat > "$CFG" <<'CLIFF'
[changelog]
header = """
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]
"""
body = """
{% if version %}\
{% if previous.version %}\
## [{{ version | trim_start_matches(pat="v") }}](https://github.com/ConductorOne/agent-tui/compare/{{ previous.version }}...{{ version }}) - {{ timestamp | date(format="%Y-%m-%d") }}
{% else %}\
## [{{ version | trim_start_matches(pat="v") }}] - {{ timestamp | date(format="%Y-%m-%d") }}
{% endif %}\
{% else %}\
## [Unreleased]
{% endif %}\
{% for group, commits in commits | group_by(attribute="group") %}
### {{ group | upper_first }}
{% for commit in commits %}
- {% if commit.scope %}*({{ commit.scope }})* {% endif %}{% if commit.breaking %}[**breaking**] {% endif %}{{ commit.message | split(pat="\n") | first | trim | upper_first }}\
{% endfor %}
{% endfor %}
"""
trim = false

[git]
protect_breaking_commits = true
# Keep pre-conventional historical commits (e.g. "P0 closure: ...", "initial
# scaffolding: ...") — they predate the conventional-commit convention but are
# real release content. git-cliff's default filter_unconventional=true would
# drop them BEFORE commit_parsers run, so the catch-all `.*` never sees them.
filter_unconventional = false
tag_pattern = "v[0-9].*"
sort_commits = "oldest"
commit_preprocessors = [
  { pattern = "\\(#([0-9]+)\\)", replace = "([#${1}](https://github.com/ConductorOne/agent-tui/pull/${1}))" },
]
commit_parsers = [
  { message = "^Merge ", skip = true },
  { message = "^chore\\(.*\\): release", skip = true },
  { message = "^chore\\(release\\)", skip = true },
  { message = "^chore: release", skip = true },
  { message = "^.*: release v[0-9]", skip = true },
  { message = "^release v[0-9]", skip = true },
  { message = "^feat", group = "Added" },
  { message = "^fix", group = "Fixed" },
  { message = "^perf", group = "Performance" },
  { message = "^refactor", group = "Changed" },
  { message = "^docs", group = "Documentation" },
  { message = "^test", group = "Testing" },
  { message = "^ci", group = "CI/CD" },
  { message = "^build", group = "Build" },
  { message = "^chore", group = "Other" },
  { message = ".*", group = "Other" },
]
CLIFF

# Header + the single static `## [Unreleased]` placeholder, emitted once.
# We deliberately do NOT render a git-cliff Unreleased section: the only
# currently-unreleased commit is the meta changelog-config commit, which must
# not appear as a user-facing change. The header text mirrors the [changelog]
# header in the config above (and the existing Keep-a-Changelog file).
cat > "$OUT" <<'HEADER'
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

HEADER

# Render each released tag from its own explicit range, newest first, so
# attribution is pinned per-release and cannot drift.
mapfile -t tags < <(git tag | sort -rV)
n=${#tags[@]}
for ((i=0; i<n; i++)); do
  tag="${tags[$i]}"
  if (( i+1 < n )); then
    # Normal case: render exactly the commits in (prev_tag, tag].
    range="${tags[$((i+1))]}..${tag}"
    "$GIT_CLIFF" --config "$CFG" --repository "$REPO" --tag "$tag" --strip all -- "$range" 2>/dev/null >> "$OUT"
  else
    # OLDEST tag: a `root..tag` range would EXCLUDE the parent-less root
    # commit ("initial scaffolding"), and git-cliff can't express a
    # root-inclusive range. But the oldest release can't suffer attribution
    # drift (nothing precedes it), so a full-history pass renders its section
    # correctly AND includes the root commit. Extract just that section.
    "$GIT_CLIFF" --config "$CFG" --repository "$REPO" --strip all 2>/dev/null \
      | awk -v t="## [${tag#v}]" 'index($0,t)==1{f=1} f' >> "$OUT"
  fi
done

echo "Wrote $OUT"
