#!/usr/bin/env bash
# Cut a release: bump the root agentflare crate's version, draft a changelog
# entry from conventional commits since the last tag, and open a PR.
#
# agentflare doesn't publish to crates.io (sub-crates are versioned/tagged
# independently), so this is intentionally the simple manual-release shape
# real single/small-maintainer Rust CLIs use (ripgrep, etc.): bump + tag +
# let CI build off the tag push. No release-plz/cargo-release dependency.
#
# Usage: scripts/release-bump.sh <new-version>   e.g. scripts/release-bump.sh 1.6.0
set -euo pipefail

cd "$(dirname "$0")/.."

error() {
  echo "$@" >&2
  exit 1
}

[[ $# -eq 1 ]] || error "Usage: $0 <new-version>  (e.g. $0 1.6.0)"
new_version="${1#v}"
[[ "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || error "version must be X.Y.Z, got: $new_version"

current_version=$(grep -m1 '^version = "' Cargo.toml | sed -E 's/version = "(.*)"/\1/')
[[ -n "$current_version" ]] || error "could not read current version from Cargo.toml"
[[ "$current_version" != "$new_version" ]] || error "new version ($new_version) matches current version"

current_branch=$(git rev-parse --abbrev-ref HEAD)
[[ "$current_branch" == "master" ]] || error "run this from master (on: $current_branch)"
git diff --quiet && git diff --cached --quiet || error "working tree not clean"

git fetch --quiet origin master
git rev-parse HEAD | grep -qx "$(git rev-parse origin/master)" || error "master is behind origin/master — pull first"

last_tag="v$current_version"
git rev-parse -q --verify "refs/tags/$last_tag" >/dev/null || error "tag $last_tag not found — is $current_version really the last release?"

branch="release/v$new_version"
git checkout -b "$branch"

sed -i "0,/^version = \"$current_version\"/s//version = \"$new_version\"/" Cargo.toml
cargo check --quiet

# Draft changelog: group conventional-commit subjects since the last tag by
# type, one section per label (Added/Fixed/Changed). This is a starting point
# for the PR, not a final answer — review and edit it before merging, same as
# any other PR.
changelog_entry=$(mktemp)
{
  echo "## [Unreleased]"
  echo
  echo "## [$new_version](https://github.com/getappz/agentflare/compare/$last_tag...v$new_version) - $(date +%Y-%m-%d)"

  any_commits=false
  for label in Added Fixed Changed; do
    case "$label" in
      Added) types="feat" ;;
      Fixed) types="fix" ;;
      Changed) types="chore|ci|refactor|docs" ;;
    esac
    section=$(git log --no-merges --pretty=format:"%s" "$last_tag..HEAD" \
      | grep -E "^(${types})(\(|:)" \
      | sed -E "s/^(${types})(\([^)]*\))?: /- /" \
      | sort -u || true)
    if [[ -n "$section" ]]; then
      any_commits=true
      echo
      echo "### $label"
      echo
      echo "$section"
    fi
  done

  if [[ "$any_commits" == false ]]; then
    echo
    echo "### Added"
    echo
    echo "- (no conventional-commit feat/fix/chore/ci/refactor/docs subjects found since $last_tag — fill in manually)"
  fi
} >"$changelog_entry"

sed -i "/^## \[Unreleased\]$/{
r $changelog_entry
d
}" CHANGELOG.md
rm -f "$changelog_entry"

git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "chore(release): agentflare v$new_version"
git push -u origin "$branch"

gh pr create \
  --title "chore(release): agentflare v$new_version" \
  --body "$(cat <<EOF
## Summary
Version bump $current_version → $new_version, via \`scripts/release-bump.sh\` (no release-plz/crates.io — this repo ships binaries only).

The changelog section below the [Unreleased] header is auto-drafted from conventional-commit subjects since $last_tag — **review and edit before merging**, it's a starting point, not a verified summary.

## Next step
After this merges, run \`mise run release:tag\` to tag and push, which triggers \`release.yml\`.
EOF
)"

echo "PR opened for v$new_version. Review/edit the changelog draft, then merge and run: mise run release:tag"
