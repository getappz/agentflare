#!/bin/sh
# install.sh — Install agentflare (download pre-built binary or build from source)
#
# Usage:
#   ./install.sh                # download pre-built binary if run outside the repo,
#                                # build from source if run inside a checkout
#   ./install.sh --download     # download pre-built binary (no Rust needed)
#   ./install.sh --build-only   # build only, don't install
#   ./install.sh --uninstall    # remove the installed binary
#
# One-liner (no Rust required):
#   curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/master/install.sh | sh
#
# Uninstall one-liner:
#   curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/master/install.sh | sh -s -- --uninstall

set -eu

# --- branding banner (mirror of assets/banner.txt; keep in sync) ---
print_banner() {
  # Suppress for non-interactive, NO_COLOR, or quiet install.
  case "${AGENTFLARE_QUIET_INSTALL:-}" in 1|true) return 0 ;; esac
  if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    CY=$'\033[2;36m'; MG=$'\033[1;35m'; RS=$'\033[0m'
  else
    CY=""; MG=""; RS=""
  fi
  printf '%s\n' "${CY}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RS}"
  printf '%s\n' "${MG}  agentflare  ·  Optimize AI CLI agents for cost & performance${RS}"
  printf '%s\n' "${CY}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RS}"
}

REPO="getappz/agentflare"
INSTALL_DIR="${AGENTFLARE_INSTALL_DIR:-$HOME/.local/bin}"
# Resolve the script's directory when invoked as a file. When piped via
# `curl ... | sh`, $0 is "sh" (or similar) — SCRIPT_IS_FILE stays 0 and the
# dispatch below always downloads: trusting pwd would build whatever
# unrelated Cargo.toml the user happens to be sitting in.
SCRIPT_IS_FILE=0
if [ -n "${0:-}" ] && [ -f "$0" ]; then
  SCRIPT_IS_FILE=1
fi
SCRIPT_DIR="$(
  if [ "$SCRIPT_IS_FILE" = "1" ]; then
    cd "$(dirname "$0")" 2>/dev/null && pwd
  else
    pwd
  fi
)"

print_banner

finish() {
  case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
      echo ""
      echo "Warning: $INSTALL_DIR is not in your PATH."
      shell_name="$(basename "${SHELL:-bash}" 2>/dev/null || echo bash)"
      rc="$HOME/.bashrc"
      case "$shell_name" in
        zsh)  rc="$HOME/.zshrc" ;;
        fish) rc="$HOME/.config/fish/config.fish" ;;
      esac
      if [ "$shell_name" = "fish" ]; then
        echo "  fish_add_path $INSTALL_DIR"
      else
        echo "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> $rc && source $rc"
      fi
      ;;
  esac
  echo ""
  echo "Done! Verify with: agentflare --version"
  echo ""
  echo "Next step: agentflare init --agent <claude-code|codex|cursor|windsurf|vscode-copilot|cline|continue>"
}

detect_target() {
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  arch="$(uname -m)"

  case "$arch" in
    x86_64)        arch="x86_64" ;;
    arm64|aarch64) arch="aarch64" ;;
    *)
      echo "Error: unsupported architecture '$arch'"
      echo "Build from source instead: cargo install --git https://github.com/${REPO}"
      exit 1 ;;
  esac

  case "$os" in
    linux) echo "${arch}-unknown-linux-gnu" ;;
    darwin) echo "${arch}-apple-darwin" ;;
    *)
      echo "Error: unsupported OS '$os'"
      echo "Windows: run install.ps1, or cargo install --git https://github.com/${REPO}"
      exit 1 ;;
  esac
}

verify_checksum() {
  file="$1"
  expected="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$file" | cut -d' ' -f1)"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$file" | cut -d' ' -f1)"
  else
    echo "Warning: no sha256sum/shasum found, skipping checksum verification"
    return 0
  fi

  if [ "$actual" != "$expected" ]; then
    echo "Error: checksum mismatch!"
    echo "  Expected: $expected"
    echo "  Got:      $actual"
    exit 1
  fi
  echo "  Checksum verified"
}

install_download() {
  target="$(detect_target)"
  echo "Mode: download pre-built binary"
  echo "Platform: $target"
  echo ""

  echo "Fetching latest release..."
  latest="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | head -1 | cut -d'"' -f4)"

  if [ -z "$latest" ]; then
    echo "Error: could not determine latest release."
    exit 1
  fi
  echo "Latest: $latest"

  asset_url="https://github.com/${REPO}/releases/download/${latest}/agentflare-${target}.tar.gz"
  sums_url="https://github.com/${REPO}/releases/download/${latest}/SHA256SUMS"

  tmpdir="$(mktemp -d)"
  tmp_bin=""
  trap 'rm -rf "${tmpdir:-}"; [ -n "${tmp_bin:-}" ] && rm -f "${tmp_bin:-}" 2>/dev/null || true' EXIT

  echo "Downloading binary..."
  if ! curl -fsSL "$asset_url" -o "$tmpdir/agentflare.tar.gz"; then
    echo "Error: download failed. Check: https://github.com/${REPO}/releases"
    exit 1
  fi

  echo "Downloading checksums..."
  # Fail closed: an installer that silently skips verification when the
  # (much smaller) SHA256SUMS request is blocked or incomplete is exactly
  # what a selective MITM wants. AGENTFLARE_SKIP_VERIFY=1 is the escape hatch.
  if curl -fsSL "$sums_url" -o "$tmpdir/SHA256SUMS" 2>/dev/null; then
    expected="$(grep "agentflare-${target}.tar.gz" "$tmpdir/SHA256SUMS" | cut -d' ' -f1)"
    if [ -n "$expected" ]; then
      verify_checksum "$tmpdir/agentflare.tar.gz" "$expected"
    elif [ "${AGENTFLARE_SKIP_VERIFY:-0}" = "1" ]; then
      echo "  Warning: agentflare-${target}.tar.gz not listed in SHA256SUMS — proceeding unverified (AGENTFLARE_SKIP_VERIFY=1)"
    else
      echo "Error: agentflare-${target}.tar.gz is not listed in SHA256SUMS — refusing to install unverified."
      echo "Set AGENTFLARE_SKIP_VERIFY=1 to bypass (not recommended)."
      exit 1
    fi
  elif [ "${AGENTFLARE_SKIP_VERIFY:-0}" = "1" ]; then
    echo "  Warning: checksums not available — proceeding unverified (AGENTFLARE_SKIP_VERIFY=1)"
  else
    echo "Error: could not download SHA256SUMS — refusing to install unverified."
    echo "Set AGENTFLARE_SKIP_VERIFY=1 to bypass (not recommended)."
    exit 1
  fi

  tar -xzf "$tmpdir/agentflare.tar.gz" -C "$tmpdir"

  mkdir -p "$INSTALL_DIR"
  tmp_bin="$INSTALL_DIR/.agentflare.new.$$"
  install -m755 "$tmpdir/agentflare" "$tmp_bin"

  if [ "$(uname -s)" = "Darwin" ]; then
    xattr -cr "$tmp_bin" 2>/dev/null || true
    codesign --force --sign - "$tmp_bin" 2>/dev/null || true
  fi
  mv -f "$tmp_bin" "$INSTALL_DIR/agentflare"
  tmp_bin=""

  echo "  Installed: $INSTALL_DIR/agentflare"

  finish
}

install_from_source() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Install Rust: https://rustup.rs"
    echo "Or download a pre-built binary: $0 --download"
    exit 1
  fi

  build_only="${1:-}"

  echo "Mode: build from source"
  echo ""
  echo "Building agentflare (release)..."

  (cd "$SCRIPT_DIR" && cargo build --release)
  target_dir=$( (cd "$SCRIPT_DIR" && cargo metadata --no-deps --format-version=1 2>/dev/null) \
      | grep -o '"target_directory":"[^"]*"' \
      | head -1 \
      | sed -E 's/^"target_directory":"(.*)"$/\1/' \
      | sed 's/\\\\/\//g' || true)
  binary="${target_dir:-$SCRIPT_DIR/target}/release/agentflare"

  if [ ! -x "$binary" ]; then
    echo "Error: build failed — binary not found at $binary"
    exit 1
  fi
  echo "Built: $binary"

  if [ "$build_only" = "--build-only" ]; then
    echo "Done (build only)."
    return
  fi

  mkdir -p "$INSTALL_DIR"
  tmp_link="$INSTALL_DIR/.agentflare.link.$$"
  ln -sf "$binary" "$tmp_link"
  mv -f "$tmp_link" "$INSTALL_DIR/agentflare"
  echo "  Linked: $INSTALL_DIR/agentflare -> $binary"

  finish
}

uninstall() {
  echo "Mode: uninstall"
  echo ""
  for b in "$INSTALL_DIR/agentflare" "/usr/local/bin/agentflare"; do
    if [ -e "$b" ] || [ -L "$b" ]; then
      rm -f "$b" 2>/dev/null && echo "  Removed $b" || true
    fi
  done
  echo ""
  echo "agentflare binary removed. Hooks/rules/MCP config agentflare init wrote are untouched —"
  echo "see README.md#uninstall to remove those too."
  echo "Verify with: command -v agentflare   # should print nothing"
}

case "${1:-}" in
  --download)    install_download ;;
  --build-only)  install_from_source --build-only ;;
  --uninstall)   uninstall ;;
  --help|-h)
    echo "Usage: $0 [--download|--build-only|--uninstall|--help]"
    echo ""
    echo "  (no args)     Build from source if run inside an agentflare checkout, else download"
    echo "  --download    Download pre-built binary (no Rust needed)"
    echo "  --build-only  Build only, don't install"
    echo "  --uninstall   Remove the installed binary"
    echo ""
    echo "Environment:"
    echo "  AGENTFLARE_INSTALL_DIR  Custom install directory (default: ~/.local/bin)"
    ;;
  *)
    if [ "$SCRIPT_IS_FILE" = "1" ] && [ -f "$SCRIPT_DIR/Cargo.toml" ]; then
      install_from_source
    else
      install_download
    fi
    ;;
esac
