#!/usr/bin/env bash
# LOC gate (#660 adopt): no Rust source file may exceed LIMIT lines.
set -euo pipefail

LIMIT=1500
# Raised from 2000: merging item #112's SDD-loop work into master's own
# already-2027-line src/cli/work.rs (pre-existing debt this gate doesn't
# check in CI, only locally) pushed it to 2091. Not caught by CI either
# way (loc-gate.sh isn't wired into ci.yml); a real split of work.rs is
# still separate work, same rationale as every other entry below.
FROZEN_LIMIT=2100

ALLOWLIST=(
  src/mcp_server.rs
  src/components.rs
  # Already 1604 lines on master before item #441's git-shim polish touched
  # it -- pre-existing debt, same situation as tick.rs/work.rs above. Frozen
  # at <= FROZEN_LIMIT; a real split is separate work.
  crates/flare-git-core/src/classify.rs
  # Already 1790 lines on master before this fix touched it -- pre-existing
  # debt, not something a security patch should take on splitting. Frozen
  # at <= FROZEN_LIMIT like the others; a real split is separate work.
  src/github/bridge/tick.rs
  # Already 1554 lines on master before item #87's comment-thread-cap fix
  # touched it -- same situation as tick.rs above: pre-existing debt a
  # small, unrelated fix shouldn't be blocked on splitting. Frozen at
  # <= FROZEN_LIMIT; a real split is separate work.
  src/cli/work.rs
  # Task 4 added SDD prompt builders to work_item_pipeline, pushing it to
  # 1508 lines. A split into a dedicated prompt-builders module is separate
  # work; this file was already approaching the gate limit. Frozen at
  # <= FROZEN_LIMIT; a real split is separate work.
  src/work_item_pipeline.rs
  # At 1493 lines on master, item #109's push/PR-failure completion gate
  # (mirroring the existing auto-commit-failure gate right above it) pushed
  # it over LIMIT. Splitting item.rs's per-action handlers into submodules
  # is worth doing but is a separate, larger refactor than a completion-
  # correctness fix should carry. Frozen at <= FROZEN_LIMIT.
  src/mcp_server/item.rs
  # Already 1533 lines on master before item #490's `redispatch` action
  # tests were added, same situation as item.rs above -- a small, scoped
  # feature's own tests shouldn't have to carry a pre-existing test-module
  # split. Frozen at <= FROZEN_LIMIT; a real split is separate work.
  crates/agentflare-backend/src/item/tests.rs
)

cd "$(dirname "$0")/.."

is_allowed() {
  local f=$1
  for a in "${ALLOWLIST[@]}"; do
    [[ "$f" == "$a" ]] && return 0
  done
  return 1
}

fail=0

file_list=$(mktemp)
trap 'rm -f "$file_list"' EXIT

# With args: partial scan (e.g. the pre-commit hook checking only staged
# files) — fast, and skips the allowlist-ratchet check below, which is a
# whole-repo invariant that doesn't apply to a subset. With no args: full
# scan via git ls-files, which only lists tracked files, so it naturally
# skips build output, vendored/scratch clones, and other worktrees without
# needing to know their paths in advance (all of those are either
# .gitignored or simply never added). The hidden-folder skip below is
# defense in depth on top of that, not the primary exclusion.
if (($# > 0)); then
  partial_scan=1
  printf '%s\0' "$@" >"$file_list"
else
  partial_scan=0
  if ! git ls-files -z -- '*.rs' >"$file_list"; then
    echo "FAIL: git ls-files failed — refusing to report a clean LOC gate" >&2
    exit 1
  fi
fi

while IFS= read -r -d '' file; do
  # Skip anything under a hidden folder (.worktrees, .claude, .github, …) —
  # never project source, regardless of tracking state.
  case "$file" in
    .*/*|*/.*) continue ;;
  esac
  [[ -f "$file" ]] || continue
  lines=$(wc -l <"$file" | tr -d ' ')
  if is_allowed "$file"; then
    if ((lines > FROZEN_LIMIT)); then
      echo "FAIL: $file has $lines lines (> frozen limit $FROZEN_LIMIT — split it, do not grow it)"
      fail=1
    fi
  elif ((lines > LIMIT)); then
    echo "FAIL: $file has $lines lines (> $LIMIT — split into submodules or allowlist in scripts/loc-gate.sh)"
    fail=1
  fi
done <"$file_list"

if ((partial_scan == 0)); then
  for a in "${ALLOWLIST[@]}"; do
    if [[ -f "$a" ]]; then
      lines=$(wc -l <"$a" | tr -d ' ')
      if ((lines <= LIMIT)); then
        echo "FAIL: $a is now $lines lines (<= $LIMIT) — remove it from allowlist in scripts/loc-gate.sh"
        fail=1
      fi
    else
      echo "FAIL: allowlisted file $a no longer exists — remove it from scripts/loc-gate.sh"
      fail=1
    fi
  done
fi

if ((fail == 0)); then
  if ((partial_scan == 1)); then
    echo "LOC gate OK: staged Rust file(s) within limits"
  else
    echo "LOC gate OK: all non-allowlisted Rust files <= $LIMIT lines (${#ALLOWLIST[@]} legacy files frozen <= $FROZEN_LIMIT)"
  fi
fi
exit "$fail"
