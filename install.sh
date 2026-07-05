#!/bin/sh
# Universal installer for leanstack (Linux/macOS). Downloads the prebuilt
# binary from GitHub Releases — same shape as rtk-ai/rtk's install.sh and
# lean-ctx's leanctx.com/install.sh. Windows: see README.md#windows.
set -e

REPO="getappz/leanstack"
INSTALL_DIR="${LEANSTACK_INSTALL_DIR:-$HOME/.local/bin}"

os() {
  case "$(uname -s)" in
    Linux) echo "unknown-linux-gnu" ;;
    Darwin) echo "apple-darwin" ;;
    *) echo "unsupported: $(uname -s)" >&2; exit 1 ;;
  esac
}

arch() {
  case "$(uname -m)" in
    x86_64|amd64) echo "x86_64" ;;
    arm64|aarch64) echo "aarch64" ;;
    *) echo "unsupported: $(uname -m)" >&2; exit 1 ;;
  esac
}

TARGET="$(arch)-$(os)"
TARBALL="leanstack-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/latest/download/${TARBALL}"

echo "Installing leanstack for ${TARGET}..."
mkdir -p "$INSTALL_DIR"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSL "$URL" -o "$TMP/$TARBALL" || {
  echo "No prebuilt binary for ${TARGET}. Try: cargo install --git https://github.com/${REPO}" >&2
  exit 1
}
tar -xzf "$TMP/$TARBALL" -C "$TMP"
mv "$TMP/leanstack" "$INSTALL_DIR/leanstack"
chmod +x "$INSTALL_DIR/leanstack"

echo "Installed to $INSTALL_DIR/leanstack"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "Add it to PATH: echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.bashrc  # or ~/.zshrc" ;;
esac
echo "Then: leanstack init --agent claude-code   (or codex/cursor/windsurf/vscode-copilot/cline/continue)"
