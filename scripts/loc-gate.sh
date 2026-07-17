#!/usr/bin/env bash
# LOC gate (#660 adopt): no Rust source file may exceed LIMIT lines.
set -euo pipefail

LIMIT=1500
FROZEN_LIMIT=2000

ALLOWLIST=(
  src/mcp_server.rs
  crates/agentflare-backend/src/item.rs
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

if ! find . -path ./target -prune -o -name '*.rs' -type f -print0 >"$file_list"; then
  echo "FAIL: find traversal failed — refusing to report a clean LOC gate" >&2
  exit 1
fi

while IFS= read -r -d '' file; do
  lines=$(wc -l <"$file" | tr -d ' ')
  relative_file=${file#./}
  if is_allowed "$relative_file"; then
    if ((lines > FROZEN_LIMIT)); then
      echo "FAIL: $file has $lines lines (> frozen limit $FROZEN_LIMIT — split it, do not grow it)"
      fail=1
    fi
  elif ((lines > LIMIT)); then
    echo "FAIL: $file has $lines lines (> $LIMIT — split into submodules or allowlist in scripts/loc-gate.sh)"
    fail=1
  fi
done <"$file_list"

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

if ((fail == 0)); then
  echo "LOC gate OK: all non-allowlisted Rust files <= $LIMIT lines (${#ALLOWLIST[@]} legacy files frozen <= $FROZEN_LIMIT)"
fi
exit "$fail"
