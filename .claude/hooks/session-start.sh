#!/bin/bash
# SessionStart hook for Claude Code on the web: installs lean-ctx (the
# context-compression layer AGENTS.md makes mandatory for code reads and
# searches) and pre-fetches Rust dependencies so tests and clippy work
# offline-fast. Idempotent; the container is cached after it completes.
#
# Install strategy (in order):
#   1. Prebuilt release tarball from the fixed
#      github.com/<repo>/releases/latest/download/<asset> URL, verified
#      against the release's SHA256SUMS. This needs neither the GitHub
#      *API* (api.github.com answers 403 from the sandbox's shared egress
#      IP, which is what breaks `npm install -g lean-ctx-bin`) nor a
#      from-source build. Takes seconds.
#   2. `cargo install lean-ctx` from crates.io as a fallback only: on the
#      4-core sandbox it compiles for well over 10 minutes, which is why
#      the hook previously timed out before it ever reached `onboard`.
#      .claude/settings.json raises the hook timeout so the fallback can
#      still finish.
set -euo pipefail

if [ "${CLAUDE_CODE_REMOTE:-}" != "true" ]; then
  exit 0
fi

LEAN_CTX_REPO="yvgude/lean-ctx"
# Pin with LEAN_CTX_VERSION=3.10.2 (a release tag without the leading v);
# unset tracks the latest release.
LEAN_CTX_VERSION="${LEAN_CTX_VERSION:-}"
BIN_DIR="${HOME}/.local/bin"

log() { printf 'session-start: %s\n' "$*" >&2; }

lean_ctx_target() {
  local arch libc="gnu"
  case "$(uname -m)" in
    x86_64 | amd64) arch="x86_64" ;;
    aarch64 | arm64) arch="aarch64" ;;
    *) return 1 ;;
  esac
  if ldd --version 2>&1 | grep -qi musl; then libc="musl"; fi
  printf '%s-unknown-linux-%s' "$arch" "$libc"
}

install_lean_ctx_prebuilt() {
  local target base asset tmp
  target="$(lean_ctx_target)" || { log "unsupported platform $(uname -m)"; return 1; }
  if [ -n "$LEAN_CTX_VERSION" ]; then
    base="https://github.com/${LEAN_CTX_REPO}/releases/download/v${LEAN_CTX_VERSION}"
  else
    base="https://github.com/${LEAN_CTX_REPO}/releases/latest/download"
  fi
  asset="lean-ctx-${target}.tar.gz"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  log "downloading ${base}/${asset}"
  # `set -e` is suppressed inside `if ! install_lean_ctx_prebuilt`, so
  # every step returns explicitly to let the cargo fallback run.
  curl -fsSL --retry 3 --retry-delay 2 -A "agentflare-session-start" \
    -o "${tmp}/${asset}" "${base}/${asset}" || return 1
  curl -fsSL --retry 3 --retry-delay 2 -A "agentflare-session-start" \
    -o "${tmp}/SHA256SUMS" "${base}/SHA256SUMS" || return 1
  (cd "$tmp" && grep -F " ${asset}" SHA256SUMS | sha256sum -c --quiet -) \
    || { log "checksum mismatch for ${asset}"; return 1; }

  tar -xzf "${tmp}/${asset}" -C "$tmp" lean-ctx || { log "failed to extract ${asset}"; return 1; }
  mkdir -p "$BIN_DIR" || return 1
  install -m 0755 "${tmp}/lean-ctx" "${BIN_DIR}/lean-ctx" || { log "failed to install to ${BIN_DIR}"; return 1; }
  log "installed $("${BIN_DIR}/lean-ctx" --version 2>/dev/null | head -1) to ${BIN_DIR}"
}

# Make $BIN_DIR visible to this script, and to the session if the hook
# runner exposes CLAUDE_ENV_FILE.
case ":${PATH}:" in
  *":${BIN_DIR}:"*) ;;
  *)
    export PATH="${BIN_DIR}:${PATH}"
    if [ -n "${CLAUDE_ENV_FILE:-}" ]; then
      printf 'export PATH="%s:$PATH"\n' "$BIN_DIR" >> "$CLAUDE_ENV_FILE"
    fi
    ;;
esac

if ! command -v lean-ctx >/dev/null 2>&1; then
  if ! install_lean_ctx_prebuilt; then
    log "prebuilt download failed; falling back to cargo install (slow)"
    cargo install lean-ctx --locked >&2
  fi
fi

# Registers the lean-ctx MCP server and hooks with Claude Code. Agent
# aliases are skipped: they'd rewrite `claude`/`codex` for every shell.
lean-ctx onboard --no-agent-aliases </dev/null >&2
# lean-ctx gates shell commands through an allowlist; these are part of
# this repo's normal format/test workflow but not on its built-in list.
lean-ctx allow rustfmt python3 timeout >&2

cd "${CLAUDE_PROJECT_DIR:-$(dirname "$0")/../..}"
cargo fetch --locked >&2
