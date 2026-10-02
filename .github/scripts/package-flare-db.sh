#!/usr/bin/env bash
# `cargo package` for flare-db resolves its pinned `flare-db-macros = "=X.Y.Z"`
# against crates.io, so the dry run can only pass once that exact macros version
# is published (release order: macros first, then flare-db). Run the dry run when
# it is; skip it with a notice when crates.io answers and does not list the
# version yet. Any other answer (network or index error) fails the step, so a
# flaky lookup can never turn the check into a silent pass.
#
# Usage: package-flare-db.sh <extra cargo package args...>
set -euo pipefail

manifest=crates/flare-db/Cargo.toml
pinned=$(sed -n 's/^flare-db-macros = .*version = "=\([^"]*\)".*/\1/p' "$manifest")
if [ -z "$pinned" ]; then
  echo "error: no exact flare-db-macros pin found in $manifest" >&2
  exit 1
fi

index=$(mktemp)
trap 'rm -f "$index"' EXIT
status=$(curl -sS -o "$index" -w '%{http_code}' https://index.crates.io/fl/ar/flare-db-macros || true)

case "$status" in
  200)
    if grep -q "\"vers\":\"$pinned\"" "$index"; then
      cargo +1.94.0 package --manifest-path "$manifest" --locked "$@"
    else
      echo "::notice::flare-db-macros $pinned is not on crates.io yet; skipping the flare-db package dry run. Publish the macros crate first, then this check runs."
    fi
    ;;
  404)
    echo "::notice::flare-db-macros is not on crates.io at all; skipping the flare-db package dry run."
    ;;
  *)
    echo "error: could not read the crates.io index for flare-db-macros (HTTP '$status')" >&2
    exit 1
    ;;
esac
