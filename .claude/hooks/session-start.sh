#!/bin/bash
# SessionStart hook for Claude Code on the web: installs lean-ctx (the
# context-compression layer AGENTS.md makes mandatory for code reads and
# searches) and pre-fetches Rust dependencies so tests and clippy work
# offline-fast. Idempotent; the container is cached after it completes.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

# npm's `lean-ctx-bin` installer downloads from the GitHub releases API,
# which the web sandbox's proxy blocks; crates.io is reachable.
if ! command -v lean-ctx >/dev/null 2>&1; then
  cargo install lean-ctx --locked >&2
fi
# Registers the lean-ctx MCP server and hooks with Claude Code. Shell
# aliases are skipped: they'd rewrite `claude`/`codex` for every shell.
lean-ctx onboard --no-agent-aliases </dev/null >&2
# lean-ctx gates shell commands through an allowlist; these are part of
# this repo's normal format/test workflow but not on its built-in list.
lean-ctx allow rustfmt python3 timeout >&2

cd "${CLAUDE_PROJECT_DIR:-$(dirname "$0")/../..}"
cargo fetch --locked >&2
