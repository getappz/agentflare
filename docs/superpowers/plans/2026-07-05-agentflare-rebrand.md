# agentflare Rebrand Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename `leanstack` to `agentflare` end-to-end (code, docs, CI, install scripts, live GitHub/Homebrew infra) and add a new `src/optimize.rs` runtime-optimizer module providing four advisory features: session hygiene nudges, model-tier routing nudges, a call-batching linter, and `ScheduleWakeup` dead-zone nudges.

**Architecture:** Phase A is a mechanical, string-for-string rename across the existing codebase (no behavior change — same components/hooks, new name). Phase B adds one new file, `src/optimize.rs`, following the exact pattern already established by `src/state.rs` (a `Load`/`Save` state struct persisted under the home dir, tested via `crate::paths::test_support::with_temp_home`) plus one new hook subcommand (`agentflare hook pre-tool-use --agent claude-code`) wired the same way `SessionStart`/`UserPromptSubmit` already are in `init.rs`.

**Tech Stack:** Rust (existing: `clap`, `serde`, `serde_json`, `dirs` — no new dependencies), `cargo test`, existing GitHub Actions workflows.

## Global Constraints

- No new Cargo dependencies (spec: "no new dependencies needed").
- All four optimizer features are advisory-only — never block/fail a tool call (spec: fail-open, same pattern as existing `state.rs`).
- No migration shim for `~/.leanstack/` → `~/.agentflare/` (spec: Non-goals — document the reset, don't code around it).
- No new marketing website in this phase (spec: Non-goals).
- Stay fully free/open, no tiering/licensing code (spec: Non-goals).
- Every new stateful function gets a unit test using the existing `crate::paths::test_support::{with_temp_home, with_temp_cwd}` helpers — no new test infra, no new dependencies (e.g. no `tempfile` crate).
- Runtime state file: `~/.agentflare/runtime-state.json`, separate from the renamed setup-state file `~/.agentflare/state.json` (spec: Approach B — separate module/state from `components.rs`'s setup concerns).

---

## Phase A: Rename mechanics

### Task 1: Rename Cargo package, binary, and core path/env constants

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/main.rs`
- Modify: `src/paths.rs`
- Modify: `src/state.rs`
- Modify: `src/hook.rs`
- Modify: `src/init.rs`
- Test: existing tests in `src/state.rs`, `src/init.rs`, `src/hook.rs` (must still pass — no test file changes needed here since they read behavior, not hardcoded strings, except where noted)

**Interfaces:**
- Consumes: nothing new
- Produces: binary renamed `agentflare`; env var `AGENTFLARE_HOME_OVERRIDE` (was `LEANSTACK_HOME_OVERRIDE`); state dir `~/.agentflare` (was `~/.leanstack`)

- [ ] **Step 1: Update `Cargo.toml`**

Replace:
```toml
[package]
name = "leanstack"
version = "1.0.0"
edition = "2021"
description = "lean-ctx + engram powered token-saving stack. Zero fluff, zero runtime deps."
license = "MIT"
repository = "https://github.com/getappz/leanstack"

[[bin]]
name = "leanstack"
path = "src/main.rs"
```
with:
```toml
[package]
name = "agentflare"
version = "1.0.0"
edition = "2021"
description = "Optimize AI CLI agents for cost and performance. Zero fluff, zero runtime deps."
license = "MIT"
repository = "https://github.com/getappz/agentflare"

[[bin]]
name = "agentflare"
path = "src/main.rs"
```

- [ ] **Step 2: Update `src/main.rs` clap app name/about and doc comment**

Replace:
```rust
#[derive(Parser)]
#[command(name = "leanstack", version, about = "lean-ctx + engram powered token-saving stack")]
struct Cli {
```
with:
```rust
#[derive(Parser)]
#[command(name = "agentflare", version, about = "Optimize AI CLI agents for cost and performance")]
struct Cli {
```

Replace:
```rust
    /// Set up leanstack for one agent: writes rules, installs lean-ctx/engram
```
with:
```rust
    /// Set up agentflare for one agent: writes rules, installs lean-ctx/engram
```

- [ ] **Step 3: Update `src/paths.rs` env var name**

Replace:
```rust
pub fn home() -> PathBuf {
    if let Ok(p) = std::env::var("LEANSTACK_HOME_OVERRIDE") {
        return PathBuf::from(p);
    }
    dirs::home_dir().expect("home directory not found")
}
```
with:
```rust
pub fn home() -> PathBuf {
    if let Ok(p) = std::env::var("AGENTFLARE_HOME_OVERRIDE") {
        return PathBuf::from(p);
    }
    dirs::home_dir().expect("home directory not found")
}
```

Also update the file's leading comment: replace the phrase `` LEANSTACK_HOME_OVERRIDE is leanstack's own escape hatch `` with `` AGENTFLARE_HOME_OVERRIDE is agentflare's own escape hatch ``.

- [ ] **Step 4: Update `src/state.rs` state directory**

Replace:
```rust
pub fn state_dir() -> PathBuf {
    home().join(".leanstack")
}
```
with:
```rust
pub fn state_dir() -> PathBuf {
    home().join(".agentflare")
}
```

- [ ] **Step 5: Update `src/hook.rs` banner and slash-command text**

Replace:
```rust
    lines.push(
        "LEANSTACK ACTIVE — lean-ctx/engram tools, Exa search, clean git commits. Off: /leanstack off."
            .to_string(),
    );
```
with:
```rust
    lines.push(
        "AGENTFLARE ACTIVE — lean-ctx/engram tools, Exa search, clean git commits. Off: /agentflare off."
            .to_string(),
    );
```

Replace:
```rust
    if prompt == "/leanstack off" || prompt == "/leanstack stop" {
        s.active = false;
        state::save(&s);
        return;
    }
    if prompt == "/leanstack on" {
        s.active = true;
        state::save(&s);
    }
```
with:
```rust
    if prompt == "/agentflare off" || prompt == "/agentflare stop" {
        s.active = false;
        state::save(&s);
        return;
    }
    if prompt == "/agentflare on" {
        s.active = true;
        state::save(&s);
    }
```

Replace:
```rust
    let mut bits = vec![
        "LEANSTACK ACTIVE.".to_string(),
```
with:
```rust
    let mut bits = vec![
        "AGENTFLARE ACTIVE.".to_string(),
```

Replace:
```rust
    if pending {
        bits.push(format!("Reminder: `leanstack init --agent {agent}` to finish setup."));
    }
```
with:
```rust
    if pending {
        bits.push(format!("Reminder: `agentflare init --agent {agent}` to finish setup."));
    }
```

- [ ] **Step 6: Update `src/init.rs` binary-name fallback and hook command strings**

Replace:
```rust
fn leanstack_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "leanstack".to_string())
}
```
with:
```rust
fn agentflare_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "agentflare".to_string())
}
```

Replace (both call sites, in `wire_claude_code` and `wire_cursor`):
```rust
    let bin = leanstack_binary();
```
with:
```rust
    let bin = agentflare_binary();
```
(there are two occurrences — one in each function; replace both)

Replace:
```rust
    let already_wired = settings
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .map(|v| v.to_string().contains("leanstack"))
        .unwrap_or(false);
```
with:
```rust
    let already_wired = settings
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .map(|v| v.to_string().contains("agentflare"))
        .unwrap_or(false);
```

Replace:
```rust
        if existing.contains("leanstack") {
            println!("  skip  .cursor/hooks.json (already wired)");
            return;
        }
        println!("  skip  .cursor/hooks.json (exists, not leanstack's — not overwriting)");
```
with:
```rust
        if existing.contains("agentflare") {
            println!("  skip  .cursor/hooks.json (already wired)");
            return;
        }
        println!("  skip  .cursor/hooks.json (exists, not agentflare's — not overwriting)");
```

- [ ] **Step 7: Run the full test suite, fix any missed rename references**

Run: `cargo test 2>&1`
Expected: `19 passed; 0 failed` (test bodies check for `"leanstack"` substring in written files in a few places — e.g. `init.rs`'s `wire_claude_code_writes_hooks_to_fresh_settings` asserts `content.contains("leanstack")` and `wire_cursor_writes_fresh_hooks_json` asserts `content.contains("leanstack")`, and `wire_cursor_does_not_clobber_foreign_hooks_file` asserts `!content.contains("leanstack")`. These now fail because the written content contains `"agentflare"` instead.)

Fix: in `src/init.rs`'s test module, replace every test assertion string `"leanstack"` with `"agentflare"`:
- `wire_claude_code_writes_hooks_to_fresh_settings`: `assert!(content.contains("leanstack"));` → `assert!(content.contains("agentflare"));`
- `wire_claude_code_preserves_existing_unrelated_settings`: `assert!(content.contains("leanstack"));` → `assert!(content.contains("agentflare"));`
- `wire_claude_code_recovers_from_corrupt_settings_file`: `assert!(content.contains("leanstack"));` → `assert!(content.contains("agentflare"));`
- `wire_cursor_writes_fresh_hooks_json`: `assert!(content.contains("leanstack"));` → `assert!(content.contains("agentflare"));`
- `wire_cursor_does_not_clobber_foreign_hooks_file`: `assert!(!content.contains("leanstack"));` → `assert!(!content.contains("agentflare"));`

Run again: `cargo test 2>&1`
Expected: `19 passed; 0 failed`

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml src/main.rs src/paths.rs src/state.rs src/hook.rs src/init.rs
git commit -m "Rename leanstack to agentflare: package, binary, env var, state dir"
```

---

### Task 2: Rename component rule-file names and codex rules header

**Files:**
- Modify: `src/components.rs`

**Interfaces:**
- Consumes: nothing new
- Produces: rule files now named `agentflare.mdc`/`agentflare.md` instead of `leanstack.mdc`/`leanstack.md`

- [ ] **Step 1: Update `rule_targets` file names and codex header text**

Replace:
```rust
        "cursor" => {
            let content = format!("---\nalwaysApply: true\n---\n\n{}", joined());
            vec![(cwd().join(".cursor").join("rules").join("leanstack.mdc"), content)]
        }
        "codex" => {
            let content = format!("# Rules (leanstack)\n\n{}\n", joined());
            vec![(cwd().join("AGENTS.md"), content)]
        }
        "windsurf" => {
            vec![(cwd().join(".windsurf").join("rules").join("leanstack.md"), joined() + "\n")]
        }
        "vscode-copilot" => {
            vec![(cwd().join(".github").join("copilot-instructions.md"), joined() + "\n")]
        }
        "cline" => {
            vec![(cwd().join(".clinerules").join("leanstack.md"), joined() + "\n")]
        }
```
with:
```rust
        "cursor" => {
            let content = format!("---\nalwaysApply: true\n---\n\n{}", joined());
            vec![(cwd().join(".cursor").join("rules").join("agentflare.mdc"), content)]
        }
        "codex" => {
            let content = format!("# Rules (agentflare)\n\n{}\n", joined());
            vec![(cwd().join("AGENTS.md"), content)]
        }
        "windsurf" => {
            vec![(cwd().join(".windsurf").join("rules").join("agentflare.md"), joined() + "\n")]
        }
        "vscode-copilot" => {
            vec![(cwd().join(".github").join("copilot-instructions.md"), joined() + "\n")]
        }
        "cline" => {
            vec![(cwd().join(".clinerules").join("agentflare.md"), joined() + "\n")]
        }
```

- [ ] **Step 2: Run tests**

Run: `cargo test 2>&1`
Expected: `19 passed; 0 failed` (existing `rule_targets_are_project_local_except_claude_code` test checks path contains e.g. `.cursor`/`.clinerules`/`AGENTS.md`/`.windsurf`/`.github`, not the literal file name — unaffected by this rename)

- [ ] **Step 3: Commit**

```bash
git add src/components.rs
git commit -m "Rename component rule-file names: leanstack -> agentflare"
```

---

### Task 3: Rename install scripts

**Files:**
- Modify: `install.sh`
- Modify: `install.ps1`

**Interfaces:**
- Consumes: nothing new
- Produces: `AGENTFLARE_INSTALL_DIR` env var (was `LEANSTACK_INSTALL_DIR`), binary name `agentflare` throughout both scripts

- [ ] **Step 1: Update `install.sh`**

Replace:
```sh
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
```
with:
```sh
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
#   curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/main/install.sh | sh
#
# Uninstall one-liner:
#   curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/main/install.sh | sh -s -- --uninstall

set -eu

REPO="getappz/agentflare"
INSTALL_DIR="${AGENTFLARE_INSTALL_DIR:-$HOME/.local/bin}"
```

Replace every remaining occurrence of the literal string `leanstack` with `agentflare` in the rest of `install.sh` — specifically these exact lines:
- `echo "leanstack installer"` → `echo "agentflare installer"`
- `echo "Done! Verify with: leanstack --version"` → `echo "Done! Verify with: agentflare --version"`
- `echo "Next step: leanstack init --agent <claude-code|codex|cursor|windsurf|vscode-copilot|cline|continue>"` → `echo "Next step: agentflare init --agent <claude-code|codex|cursor|windsurf|vscode-copilot|cline|continue>"`
- `asset_url="https://github.com/${REPO}/releases/download/${latest}/leanstack-${target}.tar.gz"` → `asset_url="https://github.com/${REPO}/releases/download/${latest}/agentflare-${target}.tar.gz"`
- `if ! curl -fsSL "$asset_url" -o "$tmpdir/leanstack.tar.gz"; then` → `if ! curl -fsSL "$asset_url" -o "$tmpdir/agentflare.tar.gz"; then`
- `expected="$(grep "leanstack-${target}.tar.gz" "$tmpdir/SHA256SUMS" | cut -d' ' -f1)"` → `expected="$(grep "agentflare-${target}.tar.gz" "$tmpdir/SHA256SUMS" | cut -d' ' -f1)"`
- `verify_checksum "$tmpdir/leanstack.tar.gz" "$expected"` → `verify_checksum "$tmpdir/agentflare.tar.gz" "$expected"`
- `tar -xzf "$tmpdir/leanstack.tar.gz" -C "$tmpdir"` → `tar -xzf "$tmpdir/agentflare.tar.gz" -C "$tmpdir"`
- `tmp_bin="$INSTALL_DIR/.leanstack.new.$$"` → `tmp_bin="$INSTALL_DIR/.agentflare.new.$$"`
- `install -m755 "$tmpdir/leanstack" "$tmp_bin"` → `install -m755 "$tmpdir/agentflare" "$tmp_bin"`
- `mv -f "$tmp_bin" "$INSTALL_DIR/leanstack"` → `mv -f "$tmp_bin" "$INSTALL_DIR/agentflare"`
- `echo "  Installed: $INSTALL_DIR/leanstack"` → `echo "  Installed: $INSTALL_DIR/agentflare"`
- `binary="${target_dir:-$SCRIPT_DIR/target}/release/leanstack"` → `binary="${target_dir:-$SCRIPT_DIR/target}/release/agentflare"`
- `tmp_link="$INSTALL_DIR/.leanstack.link.$$"` → `tmp_link="$INSTALL_DIR/.agentflare.link.$$"`
- `ln -sf "$binary" "$tmp_link"` (unchanged, no rename needed)
- `mv -f "$tmp_link" "$INSTALL_DIR/leanstack"` → `mv -f "$tmp_link" "$INSTALL_DIR/agentflare"`
- `echo "  Linked: $INSTALL_DIR/leanstack -> $binary"` → `echo "  Linked: $INSTALL_DIR/agentflare -> $binary"`
- `for b in "$INSTALL_DIR/leanstack" "/usr/local/bin/leanstack"; do` → `for b in "$INSTALL_DIR/agentflare" "/usr/local/bin/agentflare"; do`
- `echo "leanstack binary removed. Hooks/rules/MCP config leanstack init wrote are untouched —"` → `echo "agentflare binary removed. Hooks/rules/MCP config agentflare init wrote are untouched —"`
- `echo "Verify with: command -v leanstack   # should print nothing"` → `echo "Verify with: command -v agentflare   # should print nothing"`
- `echo "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> $rc && source $rc"` (unchanged, no name reference)
- `echo "  LEANSTACK_INSTALL_DIR  Custom install directory (default: ~/.local/bin)"` → `echo "  AGENTFLARE_INSTALL_DIR  Custom install directory (default: ~/.local/bin)"`

- [ ] **Step 2: Update `install.ps1`**

Replace:
```powershell
<#
install.ps1 - Build leanstack locally on Windows and install it into Cargo's bin directory.
```
with:
```powershell
<#
install.ps1 - Build agentflare locally on Windows and install it into Cargo's bin directory.
```

Replace:
```powershell
$builtBinary = Join-Path $scriptDir 'target\release\leanstack.exe'
$installedBinary = Join-Path $cargoBinDir 'leanstack.exe'

Write-Host 'leanstack Windows installer'
```
with:
```powershell
$builtBinary = Join-Path $scriptDir 'target\release\agentflare.exe'
$installedBinary = Join-Path $cargoBinDir 'agentflare.exe'

Write-Host 'agentflare Windows installer'
```

Replace:
```powershell
$tempBinary = Join-Path $cargoBinDir ('.leanstack.new.' + $PID + '.exe')
```
with:
```powershell
$tempBinary = Join-Path $cargoBinDir ('.agentflare.new.' + $PID + '.exe')
```

Replace:
```powershell
Write-Host 'Done! Verify with: leanstack --version'
Write-Host 'Next step: leanstack init --agent <claude-code|codex|cursor|windsurf|vscode-copilot|cline|continue>'
```
with:
```powershell
Write-Host 'Done! Verify with: agentflare --version'
Write-Host 'Next step: agentflare init --agent <claude-code|codex|cursor|windsurf|vscode-copilot|cline|continue>'
```

Also update the throw message referencing the repo:
Replace:
```powershell
    throw "Cargo.toml not found next to this script — run install.ps1 from a leanstack checkout, or clone https://github.com/getappz/leanstack first."
```
with:
```powershell
    throw "Cargo.toml not found next to this script — run install.ps1 from an agentflare checkout, or clone https://github.com/getappz/agentflare first."
```

- [ ] **Step 3: Verify both scripts have no remaining `leanstack` references**

Run: `grep -in leanstack install.sh install.ps1`
Expected: no output (no matches)

- [ ] **Step 4: Commit**

```bash
git add install.sh install.ps1
git commit -m "Rename install scripts: leanstack -> agentflare"
```

---

### Task 4: Rename GitHub workflow assets, SECURITY.md, and issue/PR templates

**Files:**
- Modify: `.github/workflows/release.yml`
- Modify: `SECURITY.md`
- Modify: `.github/ISSUE_TEMPLATE/bug_report.md`
- Modify: `.github/ISSUE_TEMPLATE/feature_request.md`
- Modify: `.github/ISSUE_TEMPLATE/config.yml`

**Interfaces:**
- Consumes: nothing new
- Produces: release assets named `agentflare-${target}.tar.gz`/`.zip`, Homebrew formula `Formula/agentflare.rb`, Scoop manifest `bucket/agentflare.json`, tap repo `homebrew-agentflare`

- [ ] **Step 1: Update `.github/workflows/release.yml`**

Replace every occurrence of `leanstack-${{ matrix.target }}` with `agentflare-${{ matrix.target }}` (appears in the `Package (unix)`, `Package (windows)`, and `upload-artifact` steps of the `build` job — 3 occurrences: the `tar -czf` line, the `Compress-Archive -DestinationPath` line, and the `path:` field under `upload-artifact`).

Replace:
```yaml
      - uses: actions/upload-artifact@v4
        with:
          name: leanstack-${{ matrix.target }}
          path: leanstack-${{ matrix.target }}.*
```
with:
```yaml
      - uses: actions/upload-artifact@v4
        with:
          name: agentflare-${{ matrix.target }}
          path: agentflare-${{ matrix.target }}.*
```

Replace (Package unix step):
```yaml
          tar -czf ../../../leanstack-${{ matrix.target }}.tar.gz leanstack
```
with:
```yaml
          tar -czf ../../../agentflare-${{ matrix.target }}.tar.gz agentflare
```

Replace (Package windows step):
```yaml
          Compress-Archive -Path target/${{ matrix.target }}/release/leanstack.exe -DestinationPath leanstack-${{ matrix.target }}.zip
```
with:
```yaml
          Compress-Archive -Path target/${{ matrix.target }}/release/agentflare.exe -DestinationPath agentflare-${{ matrix.target }}.zip
```

In the `update-homebrew` job, replace:
```yaml
          git clone "https://x-access-token:${HOMEBREW_TOKEN}@github.com/getappz/homebrew-leanstack.git"
          cd homebrew-leanstack
          mkdir -p Formula

          cat > Formula/leanstack.rb <<EOF
          class Leanstack < Formula
            desc "lean-ctx + engram powered token-saving stack for AI coding agents"
            homepage "https://github.com/getappz/leanstack"
            version "${VERSION}"
            license "MIT"

            on_macos do
              if Hardware::CPU.arm?
                url "https://github.com/getappz/leanstack/releases/download/v${VERSION}/leanstack-aarch64-apple-darwin.tar.gz"
                sha256 "${AARCH64_DARWIN_SHA}"
              else
                url "https://github.com/getappz/leanstack/releases/download/v${VERSION}/leanstack-x86_64-apple-darwin.tar.gz"
                sha256 "${X86_64_DARWIN_SHA}"
              end
            end

            on_linux do
              if Hardware::CPU.arm?
                url "https://github.com/getappz/leanstack/releases/download/v${VERSION}/leanstack-aarch64-unknown-linux-gnu.tar.gz"
                sha256 "${AARCH64_LINUX_SHA}"
              else
                url "https://github.com/getappz/leanstack/releases/download/v${VERSION}/leanstack-x86_64-unknown-linux-gnu.tar.gz"
                sha256 "${X86_64_LINUX_SHA}"
              end
            end

            def install
              bin.install "leanstack"
            end

            test do
              assert_match "leanstack ${VERSION}", shell_output("#{bin}/leanstack --version")
            end
          end
          EOF
```
with:
```yaml
          git clone "https://x-access-token:${HOMEBREW_TOKEN}@github.com/getappz/homebrew-agentflare.git"
          cd homebrew-agentflare
          mkdir -p Formula

          cat > Formula/agentflare.rb <<EOF
          class Agentflare < Formula
            desc "Optimize AI CLI agents for cost and performance"
            homepage "https://github.com/getappz/agentflare"
            version "${VERSION}"
            license "MIT"

            on_macos do
              if Hardware::CPU.arm?
                url "https://github.com/getappz/agentflare/releases/download/v${VERSION}/agentflare-aarch64-apple-darwin.tar.gz"
                sha256 "${AARCH64_DARWIN_SHA}"
              else
                url "https://github.com/getappz/agentflare/releases/download/v${VERSION}/agentflare-x86_64-apple-darwin.tar.gz"
                sha256 "${X86_64_DARWIN_SHA}"
              end
            end

            on_linux do
              if Hardware::CPU.arm?
                url "https://github.com/getappz/agentflare/releases/download/v${VERSION}/agentflare-aarch64-unknown-linux-gnu.tar.gz"
                sha256 "${AARCH64_LINUX_SHA}"
              else
                url "https://github.com/getappz/agentflare/releases/download/v${VERSION}/agentflare-x86_64-unknown-linux-gnu.tar.gz"
                sha256 "${X86_64_LINUX_SHA}"
              end
            end

            def install
              bin.install "agentflare"
            end

            test do
              assert_match "agentflare ${VERSION}", shell_output("#{bin}/agentflare --version")
            end
          end
          EOF
```

In the `update-scoop` job, replace:
```yaml
          WINDOWS_SHA=$(grep "x86_64-pc-windows-msvc" SHA256SUMS | awk '{print $1}')
```
(unchanged — no rename needed here, it matches the target triple not the binary name)

Replace:
```yaml
          cat > bucket/leanstack.json <<EOF
          {
            "version": "${VERSION}",
            "description": "lean-ctx + engram powered token-saving stack for AI coding agents",
            "homepage": "https://github.com/getappz/leanstack",
            "license": "MIT",
            "url": "https://github.com/getappz/leanstack/releases/download/v${VERSION}/leanstack-x86_64-pc-windows-msvc.zip",
            "hash": "sha256:${WINDOWS_SHA}",
            "bin": "leanstack.exe",
            "checkver": "github",
            "autoupdate": {
              "url": "https://github.com/getappz/leanstack/releases/download/v\$version/leanstack-x86_64-pc-windows-msvc.zip"
            }
          }
          EOF
```
with:
```yaml
          cat > bucket/agentflare.json <<EOF
          {
            "version": "${VERSION}",
            "description": "Optimize AI CLI agents for cost and performance",
            "homepage": "https://github.com/getappz/agentflare",
            "license": "MIT",
            "url": "https://github.com/getappz/agentflare/releases/download/v${VERSION}/agentflare-x86_64-pc-windows-msvc.zip",
            "hash": "sha256:${WINDOWS_SHA}",
            "bin": "agentflare.exe",
            "checkver": "github",
            "autoupdate": {
              "url": "https://github.com/getappz/agentflare/releases/download/v\$version/agentflare-x86_64-pc-windows-msvc.zip"
            }
          }
          EOF
```

Also update the checksum-download step in both `update-homebrew` and `update-scoop` jobs:
Replace every occurrence of:
```yaml
          gh release download "${GITHUB_REF_NAME}" \
            --repo getappz/leanstack --pattern "SHA256SUMS" --dir .
```
with:
```yaml
          gh release download "${GITHUB_REF_NAME}" \
            --repo getappz/agentflare --pattern "SHA256SUMS" --dir .
```
(two occurrences, one per job)

And the grep patterns extracting per-target hashes in `update-homebrew`:
Replace:
```yaml
          AARCH64_DARWIN=$(grep "aarch64-apple-darwin" SHA256SUMS | awk '{print $1}')
          X86_64_DARWIN=$(grep "x86_64-apple-darwin" SHA256SUMS | awk '{print $1}')
          AARCH64_LINUX_GNU=$(grep "aarch64-unknown-linux-gnu" SHA256SUMS | awk '{print $1}')
          X86_64_LINUX_GNU=$(grep "x86_64-unknown-linux-gnu" SHA256SUMS | awk '{print $1}')
```
(unchanged — these grep the target triple substring, which appears in the new `agentflare-*` filenames too since the triple itself didn't change; no edit needed)

- [ ] **Step 2: Update `SECURITY.md`**

Replace every occurrence of `leanstack` with `agentflare` and `getappz/leanstack` with `getappz/agentflare` throughout the file. Specifically:
- `report it privately: ... [Create a private security advisory](https://github.com/getappz/leanstack/security/advisories/new)` → `.../getappz/agentflare/security/advisories/new)`
- `leanstack is a **local-only CLI**.` → `agentflare is a **local-only CLI**.`
- `` `leanstack init --agent X` `` → `` `agentflare init --agent X` ``
- `` `leanstack hook session-start|prompt-submit` `` → `` `agentflare hook session-start|prompt-submit` ``
- `` `~/.leanstack/` `` → `` `~/.agentflare/` `` (both occurrences)
- `The main risk surface is the installer subprocess calls (`npm install -g lean-ctx-bin`, `go install`, `brew install`) — leanstack only ever invokes` → `... — agentflare only ever invokes`
- `not specific to leanstack. That's why the default Windows install` → `not specific to agentflare. That's why the default Windows install`

- [ ] **Step 3: Update `.github/ISSUE_TEMPLATE/bug_report.md`**

Replace:
```markdown
---
name: Bug Report
about: Report a bug in leanstack
title: 'bug: '
labels: bug
---

**leanstack version:** (run `leanstack --version`)
```
with:
```markdown
---
name: Bug Report
about: Report a bug in agentflare
title: 'bug: '
labels: bug
---

**agentflare version:** (run `agentflare --version`)
```

- [ ] **Step 4: Update `.github/ISSUE_TEMPLATE/feature_request.md`**

Replace:
```markdown
about: Suggest an idea for leanstack
```
with:
```markdown
about: Suggest an idea for agentflare
```

- [ ] **Step 5: Update `.github/ISSUE_TEMPLATE/config.yml`**

Replace:
```yaml
    url: https://github.com/getappz/leanstack/security/advisories/new
```
with:
```yaml
    url: https://github.com/getappz/agentflare/security/advisories/new
```

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml SECURITY.md .github/ISSUE_TEMPLATE
git commit -m "Rename release assets, security policy, and issue templates: leanstack -> agentflare"
```

---

### Task 5: Rewrite README and AGENTS.md with agentflare branding

**Files:**
- Modify: `README.md`
- Modify: `AGENTS.md`

**Interfaces:**
- Consumes: nothing new
- Produces: rebranded docs — no code behavior change

- [ ] **Step 1: Rewrite the README header/tagline**

Replace:
```markdown
<div align="center">

# leanstack

**A single Rust binary. No Node, no runtime dependencies. lean-ctx + engram
powered token-saving stack across Claude Code, Codex, Cursor, Windsurf,
VS Code, Cline, and Continue.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

</div>
```
with:
```markdown
<div align="center">

# agentflare

**Optimize AI CLI agents for cost and performance. A single Rust binary, no
Node, no runtime dependencies — across Claude Code, Codex, Cursor, Windsurf,
VS Code, Cline, and Continue.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

</div>
```

- [ ] **Step 2: Update every remaining `leanstack`/`getappz/leanstack` reference in README.md**

Search the file for the literal string `leanstack` (case-insensitive) and replace each with `agentflare`, and every `getappz/leanstack` with `getappz/agentflare`. This includes (non-exhaustive list of known occurrences from the current file — verify no others remain after editing):
- The "No plugin marketplace..." paragraph: `` `leanstack init --agent X` `` → `` `agentflare init --agent X` ``
- Install section curl one-liners: `https://raw.githubusercontent.com/getappz/leanstack/main/install.sh` → `.../getappz/agentflare/main/install.sh` (appears 3 times: install, uninstall, docs-only fallback)
- Homebrew block: `brew tap getappz/leanstack` / `brew install leanstack` → `brew tap getappz/agentflare` / `brew install agentflare`
- Scoop block: `scoop bucket add leanstack https://github.com/getappz/leanstack` / `scoop install leanstack` → `scoop bucket add agentflare https://github.com/getappz/agentflare` / `scoop install agentflare`
- Windows build-from-source block: `git clone https://github.com/getappz/leanstack` / `cd leanstack` → `git clone https://github.com/getappz/agentflare` / `cd agentflare`
- `cargo install --git https://github.com/getappz/leanstack` → `cargo install --git https://github.com/getappz/agentflare`
- "Set up an agent" section: all `leanstack init --agent X` lines → `agentflare init --agent X`
- "Each run: ... installs lean-ctx ... and engram ..." paragraph and the "engram's install safety" paragraph: any `leanstack init` reference → `agentflare init`
- Docs-only fallback curl: `https://raw.githubusercontent.com/getappz/leanstack/main/AGENTS.md` → `.../getappz/agentflare/main/AGENTS.md`
- Architecture tree diagram: no literal `leanstack` string in the tree itself (paths are relative), but check the surrounding prose for `leanstack`
- "Uninstall" section: any `leanstack` reference → `agentflare`

Verify after editing: run `grep -in leanstack README.md` (via the project's shell tool) — expect no output.

- [ ] **Step 3: Rewrite `AGENTS.md`**

Replace the entire file content:
```markdown
# leanstack rules

Static fallback for agents with no MCP support and no hook mechanism (e.g. Aider).
Everything else (Claude Code, Codex, Cursor, Windsurf, VS Code/Copilot, Cline,
Continue) gets a real integration via the `leanstack` CLI — see
https://github.com/getappz/leanstack. Use this file only if your tool isn't
one of those.
```
with:
```markdown
# agentflare rules

Static fallback for agents with no MCP support and no hook mechanism (e.g. Aider).
Everything else (Claude Code, Codex, Cursor, Windsurf, VS Code/Copilot, Cline,
Continue) gets a real integration via the `agentflare` CLI — see
https://github.com/getappz/agentflare. Use this file only if your tool isn't
one of those.
```

The rest of `AGENTS.md` (lean-ctx/engram/Exa/Git sections) references `lean-ctx`/`engram`/`Exa` project names, not `leanstack` — no further changes needed there. Verify: `grep -in leanstack AGENTS.md` — expect no output after this edit.

- [ ] **Step 4: Commit**

```bash
git add README.md AGENTS.md
git commit -m "Rebrand README and AGENTS.md: leanstack -> agentflare"
```

---

### Task 6: Live infrastructure rename (GitHub repo, Homebrew tap)

**Files:** none (infrastructure-only task, no code changes)

**Interfaces:**
- Consumes: nothing
- Produces: `getappz/leanstack` → `getappz/agentflare`, `getappz/homebrew-leanstack` → `getappz/homebrew-agentflare`

**This task performs live, user-visible infrastructure changes (renaming public GitHub repositories). Confirm with the user before running these commands — do not run unattended.**

- [ ] **Step 1: Rename the main repo**

Run: `gh repo rename agentflare --repo getappz/leanstack`
Expected: confirmation prompt (GitHub CLI asks to confirm), then success message. GitHub automatically redirects the old `getappz/leanstack` URL to `getappz/agentflare`.

- [ ] **Step 2: Update the local git remote**

Run: `git remote set-url origin https://github.com/getappz/agentflare.git`
Then verify: `git remote -v`
Expected: both `fetch` and `push` URLs show `getappz/agentflare.git`

- [ ] **Step 3: Rename the Homebrew tap repo**

Run: `gh repo rename homebrew-agentflare --repo getappz/homebrew-leanstack`
Expected: confirmation prompt, then success. Note: `brew tap getappz/agentflare` (no explicit URL) will now resolve correctly since the repo is named `homebrew-agentflare`, matching Homebrew's `homebrew-<name>` convention.

- [ ] **Step 4: Verify remote rename took effect**

Run: `gh repo view getappz/agentflare --json name,url -q '.name + " " + .url'`
Expected: `agentflare https://github.com/getappz/agentflare`

Run: `gh repo view getappz/homebrew-agentflare --json name,url -q '.name + " " + .url'`
Expected: `homebrew-agentflare https://github.com/getappz/homebrew-agentflare`

(No commit needed — this task has no local file changes.)

---

## Phase B: Runtime optimizer module

### Task 7: `src/optimize.rs` — RuntimeState load/save/prune

**Files:**
- Create: `src/optimize.rs`
- Modify: `src/main.rs` (register the new module)

**Interfaces:**
- Consumes: `crate::paths::home()` (from `src/paths.rs`), `crate::paths::test_support::with_temp_home` (test-only, from `src/paths.rs`)
- Produces: `pub struct RuntimeState { pub sessions: std::collections::HashMap<String, SessionRecord> }`, `pub struct SessionRecord { pub start_ts: u64, pub turn_count: u32, pub recent_tool_calls: Vec<ToolCallRecord> }`, `pub struct ToolCallRecord { pub name: String, pub ts: u64 }`, `pub fn runtime_state_path() -> std::path::PathBuf`, `pub fn load_runtime() -> RuntimeState`, `pub fn save_runtime(state: &RuntimeState)`, `pub fn prune_stale_sessions(state: &mut RuntimeState, now: u64)` — all consumed by Tasks 8–12

- [ ] **Step 1: Write the failing test**

Create `src/optimize.rs` with only the test module first:

```rust
use crate::paths::home;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct RuntimeState {
    #[serde(default)]
    pub sessions: HashMap<String, SessionRecord>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct SessionRecord {
    pub start_ts: u64,
    #[serde(default)]
    pub turn_count: u32,
    #[serde(default)]
    pub recent_tool_calls: Vec<ToolCallRecord>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct ToolCallRecord {
    pub name: String,
    pub ts: u64,
}

const STALE_SESSION_SECS: u64 = 24 * 60 * 60;

pub fn runtime_state_path() -> PathBuf {
    crate::state::state_dir().join("runtime-state.json")
}

pub fn load_runtime() -> RuntimeState {
    fs::read_to_string(runtime_state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_runtime(state: &RuntimeState) {
    let _ = fs::create_dir_all(crate::state::state_dir());
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = fs::write(runtime_state_path(), json + "\n");
    }
}

pub fn prune_stale_sessions(state: &mut RuntimeState, now: u64) {
    state
        .sessions
        .retain(|_, record| now.saturating_sub(record.start_ts) < STALE_SESSION_SECS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn load_defaults_to_empty_when_no_file() {
        with_temp_home(|| {
            assert!(load_runtime().sessions.is_empty());
        });
    }

    #[test]
    fn save_then_load_roundtrips() {
        with_temp_home(|| {
            let mut state = RuntimeState::default();
            state.sessions.insert(
                "sess-1".to_string(),
                SessionRecord { start_ts: 1000, turn_count: 3, recent_tool_calls: vec![] },
            );
            save_runtime(&state);
            let loaded = load_runtime();
            assert_eq!(loaded.sessions.len(), 1);
            assert_eq!(loaded.sessions["sess-1"].turn_count, 3);
        });
    }

    #[test]
    fn load_falls_back_to_default_on_corrupt_file() {
        with_temp_home(|| {
            fs::create_dir_all(crate::state::state_dir()).unwrap();
            fs::write(runtime_state_path(), "not json").unwrap();
            assert!(load_runtime().sessions.is_empty());
        });
    }

    #[test]
    fn prune_drops_sessions_older_than_24h_keeps_recent() {
        let mut state = RuntimeState::default();
        state.sessions.insert(
            "old".to_string(),
            SessionRecord { start_ts: 0, turn_count: 1, recent_tool_calls: vec![] },
        );
        state.sessions.insert(
            "recent".to_string(),
            SessionRecord { start_ts: 100_000, turn_count: 1, recent_tool_calls: vec![] },
        );
        let now = 100_100; // 100s after "recent", ~27.7h after "old"
        prune_stale_sessions(&mut state, now);
        assert!(!state.sessions.contains_key("old"));
        assert!(state.sessions.contains_key("recent"));
    }
}
```

- [ ] **Step 2: Register the module in `src/main.rs`**

Replace:
```rust
mod components;
mod engram_install;
mod hook;
mod init;
mod paths;
mod rule_text;
mod state;
```
with:
```rust
mod components;
mod engram_install;
mod hook;
mod init;
mod optimize;
mod paths;
mod rule_text;
mod state;
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test optimize 2>&1`
Expected: `4 passed; 0 failed` (all four tests in `src/optimize.rs`)

- [ ] **Step 4: Run the full suite to confirm nothing else broke**

Run: `cargo test 2>&1`
Expected: `23 passed; 0 failed` (19 existing + 4 new)

- [ ] **Step 5: Commit**

```bash
git add src/optimize.rs src/main.rs
git commit -m "Add runtime optimizer state module: load/save/prune"
```

---

### Task 8: Session hygiene nudge logic

**Files:**
- Modify: `src/optimize.rs`

**Interfaces:**
- Consumes: `SessionRecord` (from Task 7)
- Produces: `pub const SESSION_HYGIENE_TURN_THRESHOLD: u32 = 80;`, `pub const SESSION_HYGIENE_TIME_THRESHOLD_SECS: u64 = 2 * 60 * 60;`, `pub fn session_hygiene_nudge(record: &SessionRecord, now: u64) -> Option<String>` — consumed by Task 12's hook wiring

- [ ] **Step 1: Write the failing test**

Add to `src/optimize.rs`, above the existing `#[cfg(test)] mod tests` block (as new pub items) and add new test functions inside the existing `mod tests` block:

```rust
pub const SESSION_HYGIENE_TURN_THRESHOLD: u32 = 80;
pub const SESSION_HYGIENE_TIME_THRESHOLD_SECS: u64 = 2 * 60 * 60;

pub fn session_hygiene_nudge(record: &SessionRecord, now: u64) -> Option<String> {
    let elapsed = now.saturating_sub(record.start_ts);
    if record.turn_count < SESSION_HYGIENE_TURN_THRESHOLD
        && elapsed < SESSION_HYGIENE_TIME_THRESHOLD_SECS
    {
        return None;
    }
    Some(format!(
        "This session has run {} turns over {}h — consider closing it (handoff + fresh session) before context re-reads get expensive.",
        record.turn_count,
        elapsed / 3600
    ))
}
```

Add these test functions inside the existing `mod tests` block in `src/optimize.rs`:

```rust
    #[test]
    fn session_hygiene_no_nudge_below_thresholds() {
        let record = SessionRecord { start_ts: 0, turn_count: 5, recent_tool_calls: vec![] };
        assert!(session_hygiene_nudge(&record, 100).is_none());
    }

    #[test]
    fn session_hygiene_nudges_past_turn_threshold() {
        let record = SessionRecord { start_ts: 0, turn_count: 81, recent_tool_calls: vec![] };
        assert!(session_hygiene_nudge(&record, 100).is_some());
    }

    #[test]
    fn session_hygiene_nudges_past_time_threshold() {
        let record = SessionRecord { start_ts: 0, turn_count: 1, recent_tool_calls: vec![] };
        assert!(session_hygiene_nudge(&record, 2 * 60 * 60 + 1).is_some());
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test optimize 2>&1`
Expected: `7 passed; 0 failed`

- [ ] **Step 3: Commit**

```bash
git add src/optimize.rs
git commit -m "Add session hygiene nudge logic"
```

---

### Task 9: Model-tier routing keyword heuristic

**Files:**
- Modify: `src/optimize.rs`

**Interfaces:**
- Consumes: nothing new (pure string function)
- Produces: `pub fn model_routing_nudge(prompt: &str) -> Option<&'static str>` — consumed by Task 12's hook wiring

- [ ] **Step 1: Write the failing test**

Add to `src/optimize.rs`:

```rust
const LOCATE_KEYWORDS: &[&str] = &["find ", "where is", "where's", "search for", "locate "];

pub fn model_routing_nudge(prompt: &str) -> Option<&'static str> {
    let lower = prompt.to_lowercase();
    if LOCATE_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
        return Some(
            "This looks like a locate/investigate task — consider a cheap-model subagent (e.g. haiku) instead of running it inline.",
        );
    }
    None
}
```

Add test functions inside the existing `mod tests` block:

```rust
    #[test]
    fn model_routing_flags_find_prompts() {
        assert!(model_routing_nudge("find the auth handler").is_some());
    }

    #[test]
    fn model_routing_flags_where_is_prompts() {
        assert!(model_routing_nudge("where is the config loaded?").is_some());
    }

    #[test]
    fn model_routing_ignores_unrelated_prompts() {
        assert!(model_routing_nudge("refactor the payment module for clarity").is_none());
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test optimize 2>&1`
Expected: `10 passed; 0 failed`

- [ ] **Step 3: Commit**

```bash
git add src/optimize.rs
git commit -m "Add model-tier routing keyword heuristic"
```

---

### Task 10: Call-batching linter logic

**Files:**
- Modify: `src/optimize.rs`

**Interfaces:**
- Consumes: `ToolCallRecord` (from Task 7)
- Produces: `pub const BATCHABLE_TOOLS: &[&str] = &[...]`, `pub fn batching_nudge(recent_calls: &[ToolCallRecord], next_tool: &str) -> Option<String>` — consumed by Task 12's hook wiring

- [ ] **Step 1: Write the failing test**

Add to `src/optimize.rs`:

```rust
pub const BATCHABLE_TOOLS: &[&str] = &["Read", "Bash", "ctx_read", "ctx_shell"];
const BATCH_WINDOW: usize = 3;

/// Flags when the last BATCH_WINDOW calls (including the one about to run)
/// are all solo calls to the same batch-eligible tool — a sign a batch form
/// should have been used instead.
pub fn batching_nudge(recent_calls: &[ToolCallRecord], next_tool: &str) -> Option<String> {
    if !BATCHABLE_TOOLS.contains(&next_tool) {
        return None;
    }
    let tail: Vec<&str> = recent_calls
        .iter()
        .rev()
        .take(BATCH_WINDOW - 1)
        .map(|c| c.name.as_str())
        .collect();
    if tail.len() < BATCH_WINDOW - 1 {
        return None;
    }
    if tail.iter().all(|&name| name == next_tool) {
        return Some(format!(
            "That's {} consecutive solo calls to {next_tool} — check if it accepts a batch/list form instead.",
            BATCH_WINDOW
        ));
    }
    None
}
```

Add test functions inside the existing `mod tests` block:

```rust
    #[test]
    fn batching_flags_three_consecutive_solo_calls() {
        let recent = vec![
            ToolCallRecord { name: "Read".to_string(), ts: 1 },
            ToolCallRecord { name: "Read".to_string(), ts: 2 },
        ];
        assert!(batching_nudge(&recent, "Read").is_some());
    }

    #[test]
    fn batching_ignores_non_batchable_tool() {
        let recent = vec![
            ToolCallRecord { name: "Write".to_string(), ts: 1 },
            ToolCallRecord { name: "Write".to_string(), ts: 2 },
        ];
        assert!(batching_nudge(&recent, "Write").is_none());
    }

    #[test]
    fn batching_ignores_mixed_recent_calls() {
        let recent = vec![
            ToolCallRecord { name: "Read".to_string(), ts: 1 },
            ToolCallRecord { name: "Grep".to_string(), ts: 2 },
        ];
        assert!(batching_nudge(&recent, "Read").is_none());
    }

    #[test]
    fn batching_ignores_short_history() {
        let recent = vec![ToolCallRecord { name: "Read".to_string(), ts: 1 }];
        assert!(batching_nudge(&recent, "Read").is_none());
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test optimize 2>&1`
Expected: `14 passed; 0 failed`

- [ ] **Step 3: Commit**

```bash
git add src/optimize.rs
git commit -m "Add call-batching linter logic"
```

---

### Task 11: ScheduleWakeup dead-zone check

**Files:**
- Modify: `src/optimize.rs`

**Interfaces:**
- Consumes: nothing new (pure numeric function)
- Produces: `pub fn schedule_wakeup_nudge(delay_seconds: u64) -> Option<&'static str>` — consumed by Task 12's hook wiring

- [ ] **Step 1: Write the failing test**

Add to `src/optimize.rs`:

```rust
pub fn schedule_wakeup_nudge(delay_seconds: u64) -> Option<&'static str> {
    if (271..300).contains(&delay_seconds) {
        return Some(
            "This delay is in the cache-miss dead zone (271-299s) — drop under 270s to stay in cache, or extend past 1200s to make the miss worth it.",
        );
    }
    None
}
```

Add test functions inside the existing `mod tests` block:

```rust
    #[test]
    fn schedule_wakeup_nudges_dead_zone() {
        assert!(schedule_wakeup_nudge(280).is_some());
    }

    #[test]
    fn schedule_wakeup_silent_under_dead_zone() {
        assert!(schedule_wakeup_nudge(200).is_none());
    }

    #[test]
    fn schedule_wakeup_silent_over_dead_zone() {
        assert!(schedule_wakeup_nudge(1500).is_none());
    }
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test optimize 2>&1`
Expected: `17 passed; 0 failed`

- [ ] **Step 3: Commit**

```bash
git add src/optimize.rs
git commit -m "Add ScheduleWakeup dead-zone check"
```

---

### Task 12: Wire `pre-tool-use` hook and integrate all four features

**Files:**
- Modify: `src/main.rs` (new `HookEvent::PreToolUse` variant)
- Modify: `src/hook.rs` (new `pre_tool_use` handler; wire session hygiene + model routing into existing handlers)
- Modify: `src/init.rs` (wire `PreToolUse` into `~/.claude/settings.json`)

**Interfaces:**
- Consumes: `crate::optimize::{load_runtime, save_runtime, prune_stale_sessions, session_hygiene_nudge, model_routing_nudge, batching_nudge, schedule_wakeup_nudge, RuntimeState, SessionRecord, ToolCallRecord}` (all from Tasks 7–11)
- Produces: `agentflare hook pre-tool-use --agent claude-code` CLI subcommand; `PreToolUse` entry in `~/.claude/settings.json`

- [ ] **Step 1: Add the new `HookEvent::PreToolUse` variant in `src/main.rs`**

Replace:
```rust
#[derive(Subcommand)]
enum HookEvent {
    SessionStart {
        #[arg(long, value_enum)]
        agent: Agent,
    },
    PromptSubmit {
        #[arg(long, value_enum)]
        agent: Agent,
    },
}
```
with:
```rust
#[derive(Subcommand)]
enum HookEvent {
    SessionStart {
        #[arg(long, value_enum)]
        agent: Agent,
    },
    PromptSubmit {
        #[arg(long, value_enum)]
        agent: Agent,
    },
    PreToolUse {
        #[arg(long, value_enum)]
        agent: Agent,
    },
}
```

Replace:
```rust
        Commands::Hook { event } => match event {
            HookEvent::SessionStart { agent } => hook::session_start(agent.as_str()),
            HookEvent::PromptSubmit { agent } => hook::prompt_submit(agent.as_str()),
        },
```
with:
```rust
        Commands::Hook { event } => match event {
            HookEvent::SessionStart { agent } => hook::session_start(agent.as_str()),
            HookEvent::PromptSubmit { agent } => hook::prompt_submit(agent.as_str()),
            HookEvent::PreToolUse { agent } => hook::pre_tool_use(agent.as_str()),
        },
```

- [ ] **Step 2: Write the failing test for `hook::pre_tool_use`'s stdin-parsing helper**

Add to `src/hook.rs`, a pure helper mirroring the existing `extract_prompt` pattern, plus its test:

```rust
struct PreToolUseInput {
    session_id: String,
    tool_name: String,
    delay_seconds: Option<u64>,
}

fn parse_pre_tool_use(input: &str) -> Option<PreToolUseInput> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    let tool_name = v.get("tool_name")?.as_str()?.to_string();
    let delay_seconds = v
        .get("tool_input")
        .and_then(|ti| ti.get("delaySeconds"))
        .and_then(|d| d.as_u64());
    Some(PreToolUseInput { session_id, tool_name, delay_seconds })
}
```

Add to the existing `#[cfg(test)] mod tests` block in `src/hook.rs`:

```rust
    #[test]
    fn parse_pre_tool_use_reads_session_and_tool_name() {
        let input = r#"{"session_id": "abc", "tool_name": "Read", "tool_input": {}}"#;
        let parsed = parse_pre_tool_use(input).unwrap();
        assert_eq!(parsed.session_id, "abc");
        assert_eq!(parsed.tool_name, "Read");
        assert_eq!(parsed.delay_seconds, None);
    }

    #[test]
    fn parse_pre_tool_use_reads_delay_seconds_for_schedule_wakeup() {
        let input = r#"{"session_id": "abc", "tool_name": "ScheduleWakeup", "tool_input": {"delaySeconds": 280}}"#;
        let parsed = parse_pre_tool_use(input).unwrap();
        assert_eq!(parsed.delay_seconds, Some(280));
    }

    #[test]
    fn parse_pre_tool_use_returns_none_on_invalid_json() {
        assert!(parse_pre_tool_use("not json").is_none());
    }
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test hook:: 2>&1`
Expected: `9 passed; 0 failed` (6 existing `extract_prompt` tests + 3 new)

- [ ] **Step 4: Implement `pre_tool_use` handler in `src/hook.rs`**

Add the full handler function to `src/hook.rs` (after `parse_pre_tool_use` and before the `#[cfg(test)]` block):

```rust
pub fn pre_tool_use(_agent: &str) {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let Some(parsed) = parse_pre_tool_use(&input) else { return };

    let mut runtime = crate::optimize::load_runtime();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::optimize::prune_stale_sessions(&mut runtime, now);

    let record = runtime
        .sessions
        .entry(parsed.session_id.clone())
        .or_insert_with(|| crate::optimize::SessionRecord {
            start_ts: now,
            turn_count: 0,
            recent_tool_calls: vec![],
        });

    let mut nudges: Vec<String> = vec![];

    if let Some(nudge) = crate::optimize::batching_nudge(&record.recent_tool_calls, &parsed.tool_name) {
        nudges.push(nudge);
    }

    if parsed.tool_name == "ScheduleWakeup" {
        if let Some(delay) = parsed.delay_seconds {
            if let Some(nudge) = crate::optimize::schedule_wakeup_nudge(delay) {
                nudges.push(nudge.to_string());
            }
        }
    }

    record.recent_tool_calls.push(crate::optimize::ToolCallRecord {
        name: parsed.tool_name.clone(),
        ts: now,
    });
    if record.recent_tool_calls.len() > 10 {
        record.recent_tool_calls.remove(0);
    }

    crate::optimize::save_runtime(&runtime);

    if !nudges.is_empty() {
        let out = json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": nudges.join(" "),
            }
        });
        println!("{out}");
    }
}
```

- [ ] **Step 5: Wire session hygiene and model routing into `prompt_submit`**

Replace:
```rust
pub fn prompt_submit(agent: &str) {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let prompt = extract_prompt(&input);
    let prompt = prompt.trim();

    let mut s = state::load();
```
with:
```rust
pub fn prompt_submit(agent: &str) {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let prompt = extract_prompt(&input);
    let prompt = prompt.trim();

    let session_id: Option<String> = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| v.get("session_id").and_then(|s| s.as_str()).map(String::from));

    let mut s = state::load();
```

Replace:
```rust
    let mut bits = vec![
        "AGENTFLARE ACTIVE.".to_string(),
        "Prefer lean-ctx ctx_* tools over native Read/Grep/Bash/Glob.".to_string(),
        "Exa is the only web search tool.".to_string(),
        "Clean git commits, no AI signature.".to_string(),
    ];
    let pending = get_components(agent)
        .iter()
        .any(|c| c.needs_consent && !(c.check)());
    if pending {
        bits.push(format!("Reminder: `agentflare init --agent {agent}` to finish setup."));
    }
```
with:
```rust
    let mut bits = vec![
        "AGENTFLARE ACTIVE.".to_string(),
        "Prefer lean-ctx ctx_* tools over native Read/Grep/Bash/Glob.".to_string(),
        "Exa is the only web search tool.".to_string(),
        "Clean git commits, no AI signature.".to_string(),
    ];
    let pending = get_components(agent)
        .iter()
        .any(|c| c.needs_consent && !(c.check)());
    if pending {
        bits.push(format!("Reminder: `agentflare init --agent {agent}` to finish setup."));
    }

    if let Some(nudge) = crate::optimize::model_routing_nudge(prompt) {
        bits.push(nudge.to_string());
    }

    if let Some(sid) = session_id {
        let mut runtime = crate::optimize::load_runtime();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        crate::optimize::prune_stale_sessions(&mut runtime, now);
        let record = runtime
            .sessions
            .entry(sid)
            .or_insert_with(|| crate::optimize::SessionRecord {
                start_ts: now,
                turn_count: 0,
                recent_tool_calls: vec![],
            });
        record.turn_count += 1;
        if let Some(nudge) = crate::optimize::session_hygiene_nudge(record, now) {
            bits.push(nudge);
        }
        crate::optimize::save_runtime(&runtime);
    }
```

- [ ] **Step 6: Wire `PreToolUse` into `~/.claude/settings.json` in `src/init.rs`**

Replace:
```rust
    hooks_obj.entry("UserPromptSubmit").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "hooks": [{ "type": "command", "command": format!("\"{bin}\" hook prompt-submit --agent claude-code"), "timeout": 5 }]
    }));

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(&path, serde_json::to_string_pretty(&settings).unwrap() + "\n") {
        Ok(_) => println!("  ok    ~/.claude/settings.json hooks wired"),
        Err(e) => println!("  fail  writing ~/.claude/settings.json: {e}"),
    }
}
```
with:
```rust
    hooks_obj.entry("UserPromptSubmit").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "hooks": [{ "type": "command", "command": format!("\"{bin}\" hook prompt-submit --agent claude-code"), "timeout": 5 }]
    }));
    hooks_obj.entry("PreToolUse").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "hooks": [{ "type": "command", "command": format!("\"{bin}\" hook pre-tool-use --agent claude-code"), "timeout": 5 }]
    }));

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(&path, serde_json::to_string_pretty(&settings).unwrap() + "\n") {
        Ok(_) => println!("  ok    ~/.claude/settings.json hooks wired"),
        Err(e) => println!("  fail  writing ~/.claude/settings.json: {e}"),
    }
}
```

- [ ] **Step 7: Update the existing idempotency test to cover the new hook**

The existing test `wire_claude_code_writes_hooks_to_fresh_settings` in `src/init.rs` already asserts `content.contains("SessionStart")` and `content.contains("UserPromptSubmit")`. Add a third assertion. Replace:
```rust
    fn wire_claude_code_writes_hooks_to_fresh_settings() {
        with_temp_home(|| {
            wire_claude_code();
            let content = fs::read_to_string(home().join(".claude").join("settings.json")).unwrap();
            assert!(content.contains("agentflare"));
            assert!(content.contains("SessionStart"));
            assert!(content.contains("UserPromptSubmit"));
        });
    }
```
with:
```rust
    fn wire_claude_code_writes_hooks_to_fresh_settings() {
        with_temp_home(|| {
            wire_claude_code();
            let content = fs::read_to_string(home().join(".claude").join("settings.json")).unwrap();
            assert!(content.contains("agentflare"));
            assert!(content.contains("SessionStart"));
            assert!(content.contains("UserPromptSubmit"));
            assert!(content.contains("PreToolUse"));
        });
    }
```

(The existing `wire_claude_code_is_idempotent` test already re-runs `wire_claude_code()` twice and asserts byte-identical output, which covers the new hook's idempotency automatically — no separate idempotency test needed.)

- [ ] **Step 8: Run the full test suite**

Run: `cargo test 2>&1`
Expected: `29 passed; 0 failed` (23 from Task 7 + 3 `session_hygiene` (Task 8) + 3 `model_routing` (Task 9) + 4 `batching` (Task 10) + 3 `schedule_wakeup` (Task 11) — wait, recompute: 19 original + 4 (Task7) + 3 (Task8) + 3 (Task9) + 4 (Task10) + 3 (Task11) + 3 (Task12 parse tests) = 39 total. Run the command and use its actual reported count rather than this arithmetic if they differ — the important thing is `0 failed`.)

Run: `cargo test 2>&1 | tail -5`
Expected: last line reads `N passed; 0 failed; 0 ignored; 0 measured; 0 filtered out` for some N, with `0 failed`.

- [ ] **Step 9: Commit**

```bash
git add src/main.rs src/hook.rs src/init.rs
git commit -m "Wire pre-tool-use hook: batching linter, ScheduleWakeup dead-zone, session hygiene, model routing"
```

---

## After this plan

Once all 12 tasks are committed and `cargo test` passes fully:
1. Push to `master`: `git push` (branch protection allows direct admin pushes; required status checks only gate PR merges, per this repo's existing config).
2. Watch the `ci` and `security-check` GitHub Actions runs go green on the new `agentflare` name.
3. Cut a new release tag (e.g. `v1.1.0`) to exercise the renamed `release.yml` end-to-end (Homebrew formula + Scoop manifest under the new name) — this is a separate, already-established workflow from earlier in this project's history, not a new task here.
