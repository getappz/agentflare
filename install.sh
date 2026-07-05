#!/bin/sh
# install.sh — Install leanstack (download pre-built binary or build from source)
#
# Usage:
#   ./install.sh                # download pre-built binary if run outside the repo,
#                                # build from source if run inside a checkout
#   ./install.sh --download     # download pre-built binary (no Rust needed)
#   ./install.sh --build-only   # build only, don't install
#   ./install.sh --uninstall    # remove the installed binary
#
# One-liner (no Rust required):
#   curl -fsSL https://raw.githubusercontent.com/getappz/leanstack/main/install.sh | sh
#
# Uninstall one-liner:
#   curl -fsSL https://raw.githubusercontent.com/getappz/leanstack/main/install.sh | sh -s -- --uninstall

set -eu

REPO="getappz/leanstack"
INSTALL_DIR="${LEANSTACK_INSTALL_DIR:-$HOME/.local/bin}"
# Resolve the script's directory when invoked as a file. When piped via
# `curl ... | sh`, $0 is "sh" (or similar) — the [ -f "$0" ] guard then
# falls back to pwd, which routes to install_download since Cargo.toml
# won't be found there.
SCRIPT_DIR="$(
  src="$0"
  if [ -n "$src" ] && [ -f "$src" ]; then
    cd "$(dirname "$src")" 2>/dev/null && pwd
  else
    pwd
  fi
)"

echo "leanstack installer"

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
  echo "Done! Verify with: leanstack --version"
  echo ""
  echo "Next step: leanstack init --agent <claude-code|codex|cursor|windsurf|vscode-copilot|cline|continue>"
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

  asset_url="https://github.com/${REPO}/releases/download/${latest}/leanstack-${target}.tar.gz"
  sums_url="https://github.com/${REPO}/releases/download/${latest}/SHA256SUMS"

  tmpdir="$(mktemp -d)"
  tmp_bin=""
  trap 'rm -rf "${tmpdir:-}"; [ -n "${tmp_bin:-}" ] && rm -f "${tmp_bin:-}" 2>/dev/null || true' EXIT

  echo "Downloading binary..."
  if ! curl -fsSL "$asset_url" -o "$tmpdir/leanstack.tar.gz"; then
    echo "Error: download failed. Check: https://github.com/${REPO}/releases"
    exit 1
  fi

  echo "Downloading checksums..."
  if curl -fsSL "$sums_url" -o "$tmpdir/SHA256SUMS" 2>/dev/null; then
    expected="$(grep "leanstack-${target}.tar.gz" "$tmpdir/SHA256SUMS" | cut -d' ' -f1)"
    if [ -n "$expected" ]; then
      verify_checksum "$tmpdir/leanstack.tar.gz" "$expected"
    fi
  else
    echo "  Warning: checksums not available, skipping verification"
  fi

  tar -xzf "$tmpdir/leanstack.tar.gz" -C "$tmpdir"

  mkdir -p "$INSTALL_DIR"
  tmp_bin="$INSTALL_DIR/.leanstack.new.$$"
  install -m755 "$tmpdir/leanstack" "$tmp_bin"

  if [ "$(uname -s)" = "Darwin" ]; then
    xattr -cr "$tmp_bin" 2>/dev/null || true
    codesign --force --sign - "$tmp_bin" 2>/dev/null || true
  fi
  mv -f "$tmp_bin" "$INSTALL_DIR/leanstack"
  tmp_bin=""

  echo "  Installed: $INSTALL_DIR/leanstack"

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
  echo "Building leanstack (release)..."

  (cd "$SCRIPT_DIR" && cargo build --release)
  target_dir=$( (cd "$SCRIPT_DIR" && cargo metadata --no-deps --format-version=1 2>/dev/null) \
      | grep -o '"target_directory":"[^"]*"' \
      | head -1 \
      | sed -E 's/^"target_directory":"(.*)"$/\1/' \
      | sed 's/\\\\/\//g' || true)
  binary="${target_dir:-$SCRIPT_DIR/target}/release/leanstack"

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
  tmp_link="$INSTALL_DIR/.leanstack.link.$$"
  ln -sf "$binary" "$tmp_link"
  mv -f "$tmp_link" "$INSTALL_DIR/leanstack"
  echo "  Linked: $INSTALL_DIR/leanstack -> $binary"

  finish
}

uninstall() {
  echo "Mode: uninstall"
  echo ""
  for b in "$INSTALL_DIR/leanstack" "/usr/local/bin/leanstack"; do
    if [ -e "$b" ] || [ -L "$b" ]; then
      rm -f "$b" 2>/dev/null && echo "  Removed $b" || true
    fi
  done
  echo ""
  echo "leanstack binary removed. Hooks/rules/MCP config leanstack init wrote are untouched —"
  echo "see README.md#uninstall to remove those too."
  echo "Verify with: command -v leanstack   # should print nothing"
}

case "${1:-}" in
  --download)    install_download ;;
  --build-only)  install_from_source --build-only ;;
  --uninstall)   uninstall ;;
  --help|-h)
    echo "Usage: $0 [--download|--build-only|--uninstall|--help]"
    echo ""
    echo "  (no args)     Build from source if run inside a leanstack checkout, else download"
    echo "  --download    Download pre-built binary (no Rust needed)"
    echo "  --build-only  Build only, don't install"
    echo "  --uninstall   Remove the installed binary"
    echo ""
    echo "Environment:"
    echo "  LEANSTACK_INSTALL_DIR  Custom install directory (default: ~/.local/bin)"
    ;;
  *)
    if [ -f "$SCRIPT_DIR/Cargo.toml" ]; then
      install_from_source
    else
      install_download
    fi
    ;;
esac
