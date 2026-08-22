#!/usr/bin/env bash
# Cache for the repo-compare workflow: a comparison verdict is only valid
# while BOTH the target repo and agentflare itself are unchanged, since
# `compare` judges the target against agentflare's current code.
#
# Usage:
#   repo-compare-cache.sh check <repo_path> <repo_name>
#     Prints the cached artifact URL and exits 0 on a hit (both SHAs match).
#     Prints nothing and exits 1 on a miss.
#   repo-compare-cache.sh write <repo_path> <repo_name> <artifact_url>
#     Records the current SHA pair + artifact URL for <repo_name>.
set -euo pipefail

cmd="${1:?usage: repo-compare-cache.sh check|write <repo_path> <repo_name> [artifact_url]}"
repo_path="${2:?repo_path required}"
repo_name="${3:?repo_name required}"

agentflare_root="$(git rev-parse --show-toplevel)"
cache_dir="$HOME/.agentflare/repo-compare-cache"
cache_file="$cache_dir/$repo_name.json"

target_sha="$(git -C "$repo_path" rev-parse HEAD)"
agentflare_sha="$(git -C "$agentflare_root" rev-parse HEAD)"

case "$cmd" in
  check)
    if [ -f "$cache_file" ]; then
      cached_target="$(jq -r '.target_sha' "$cache_file")"
      cached_agentflare="$(jq -r '.agentflare_sha' "$cache_file")"
      if [ "$cached_target" = "$target_sha" ] && [ "$cached_agentflare" = "$agentflare_sha" ]; then
        jq -r '.artifact_url' "$cache_file"
        exit 0
      fi
    fi
    exit 1
    ;;
  write)
    artifact_url="${4:?artifact_url required for write}"
    mkdir -p "$cache_dir"
    jq -n \
      --arg target_sha "$target_sha" \
      --arg agentflare_sha "$agentflare_sha" \
      --arg artifact_url "$artifact_url" \
      --arg cached_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      '{target_sha: $target_sha, agentflare_sha: $agentflare_sha, artifact_url: $artifact_url, cached_at: $cached_at}' \
      > "$cache_file"
    ;;
  *)
    echo "unknown command: $cmd (expected check|write)" >&2
    exit 2
    ;;
esac
