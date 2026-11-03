---
title: Installation and verification
description: Installation and first use, plus the command and verification inventory.
---

## Installation and first use

Installation commands below are verified against local scripts/configuration;
installations and downloads were **not executed** during this documentation pass.

| Path | Command | Actual behavior / requirements |
|---|---|---|
| Linux/macOS prebuilt | `curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/master/install.sh \| sh` | Downloads a release outside a checkout; Linux GNU or macOS, x86_64/aarch64. Needs download/archive tools; default destination `~/.local/bin`. [installer](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/install.sh#L78) |
| Unix checkout | `sh ./install.sh` or `sh ./install.sh --build-only` | Runs `cargo build --release`; normal installation atomically replaces a symlink to the built binary. `--download` forces release download. [source path](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/install.sh#L192) |
| Windows checkout | `git clone https://github.com/getappz/agentflare.git`, then `cd agentflare`, then `.\install.ps1` | Requires Cargo and a working native Rust/linker toolchain. Builds `--release`, copies to `$CARGO_HOME/bin` or `~/.cargo/bin`; `-BuildOnly` skips the copy. [script](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/install.ps1#L47) |
| Cargo Git install | `cargo install --git https://github.com/getappz/agentflare` | Installs from source; root package is `publish = false`, so this is a Git install, not a crates.io root-package install. [manifest](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L8), [README](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/README.md#install-the-cli) |
| Homebrew / Scoop | `brew tap getappz/agentflare` then `brew install agentflare`; or `scoop bucket add agentflare https://github.com/getappz/agentflare` then `scoop install agentflare` | Release automation generates package metadata. Remote package availability and installed behavior are [UNVERIFIED] here. [release workflow](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/release.yml), [Scoop manifest](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/bucket/agentflare.json) |
| Agent integration | `agentflare init --agent codex` (or another supported host) | Checks/applies components, merges host hooks/config, can install missing tools and refresh rules. Restart the host afterward. [init](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/init.rs#L92), [components](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/components.rs#L713) |

Run `agentflare --version`, `agentflare init --help`, and `agentflare doctor`
to inspect the installed version, supported host choices, and wiring. Only
`--version` and help were run here; `doctor` is a diagnostic entry point verified
in [CLI dispatch](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mod.rs#L97).

Important installation distinctions:

- **Features differ:** source installers run `cargo build --release` without
  `semantic`; release CI builds with `--features semantic`. Use
  `cargo build --release --features semantic` for that feature in a local build.
  FTS remains available without embeddings. [Unix](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/install.sh#L205),
  [Windows](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/install.ps1#L75), [release](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/release.yml#L46),
  [feature declaration](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L123).
- **Checksums are conditional:** the Unix installer checks SHA-256 when
  `sha256sum` or `shasum` is available; it warns and skips verification if neither
  exists. Windows source installation does not download a release archive.
  Release CI generates checksums, cosign bundles, and a provenance job; signatures
  are not automatically verified by the source installers.
  [checksum code](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/install.sh#L101), [release pipeline](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/release.yml#L98).
- **The binary is not every capability's only prerequisite:** lean-ctx is a
  separate integration; browser commands can install mise, a floating
  `agent-browser@latest`, and Chrome for Testing. `--no-auto-install` disables
  browser first-use installation. Agent execution additionally needs the selected
  agent and its credentials. [components](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/components.rs#L713),
  [browser installer](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/browser_install.rs#L1), [agent sender](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/workflow.rs#L66).
- **Setup updates existing configuration:** `init` is check-first, but it merges
  hook entries and can refresh managed rules; “nothing is created if it already
  exists” is not a complete description. Codex receives `~/.codex/hooks.json`
  lifecycle hooks as well as rules/MCP wiring. [init flow](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/init.rs#L92),
  [Codex hooks](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/init.rs#L644), [rule targets](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/components.rs#L287).
- **Windows install path assumption:** `install.ps1` looks specifically in
  `target/release/agentflare.exe`. A customized target directory may put the build
  elsewhere; unlike the Unix script, the Windows script does not resolve it via
  Cargo metadata. [Windows](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/install.ps1#L64), [Unix](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/install.sh#L206).

For removal, `install.sh --uninstall` removes binaries but leaves host
configuration behind. The separate `agentflare uninstall` CLI owns configuration
cleanup. [shell uninstall](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/install.sh#L233), [CLI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mod.rs#L211).

## Commands and verification inventory

**S** = verified in local source/task configuration, **H** = help/parse surface
executed successfully, **R** = read-only command executed successfully.
S/H do not mean the build, installation, server, or workflow was exercised.
All commands run from the repository root unless stated otherwise.

| Command | Purpose | Evidence / verification |
|---|---|---|
| `agentflare --version`; `agentflare --help` | Installed binary identity and command inventory | R/H; observed `1.8.1 x86_64-pc-windows-msvc (2026-10-26)`; [clap entry](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mod.rs#L74) |
| `cargo metadata --offline --locked --no-deps --format-version 1` | Inspect workspace without dependency resolution/download | R; 32 packages; [workspace](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L1) |
| `cargo build` | Default source development build | S; [contributor setup](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/CONTRIBUTING.md#setup) |
| `cargo build --release --features semantic` | Release-style native feature selection | S; [release.yml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/release.yml#L46) |
| `cargo run -- --help` | Run the source CLI after building | S; [binary target](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/Cargo.toml#L39), [entry](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/main.rs#L96) |
| `agentflare mcp`; `agentflare mcp --http` | Stdio / shared HTTP MCP | S/H; [CLI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mcp.rs#L6); neither server started |
| `agentflare serve`; `agentflare daemon start` | Start execution/dashboard foreground / background | S/H for `serve`; [serve](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/serve.rs#L27), [daemon command](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/daemon.rs#L97); not started |
| `agentflare workflow run <file-or-name> --input "…"` | JSON workflow run | S/H; [resolver](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/workflow.rs#L393), [run implementation](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/workflow.rs#L449) |
| `agentflare workflow schedule create daily --workflow <name> --every 1d` | Register an interval schedule; template requires a real workflow name | S/H; [schedule CLI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/workflow.rs), [scheduler](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/schedule/mod.rs#L1) |
| `agentflare workflow schedule fire <name-or-id>` | Queue a manual occurrence for the daemon | S/H on parent schedule help; [schedule administration](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/schedule/admin.rs); not fired |
| `agentflare browser --help`; `agentflare optimize --help`; `agentflare flare --help` | Inspect browser and optimization surfaces; legacy `flare` alias | H; [dispatch](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/src/cli/mod.rs#L86); no browser or compression run |
| `.\install.ps1 -Help` | Windows installer usage without installing | H; [script](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/install.ps1#L38) |
| `mise run verify` | Local formatter, clippy, workspace tests | S; [mise task](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/mise.toml#L31); not run |
| `cargo fmt --check` | Rust formatting | S; [CI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L199) |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -A unsafe_code -A clippy::pedantic` | Type/lint gate across targets/features | S; [CI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L179) |
| `cargo test --workspace` | Local workspace tests, including doctests | S; [mise](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/mise.toml#L31) |
| `cargo test -p agentflare-store retrieval::tests::single_bm25_result_normalizes_to_one -- --exact` | One existing unit test | S; [test](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/crates/agentflare-store/src/retrieval.rs#L131); not run |
| `cargo nextest run --workspace --verbose` | CI compilation and test binaries | S; [CI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L129) |
| `cargo test --doc --workspace --verbose` | Doctests omitted by nextest | S; [CI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L152) |
| `cargo hakari generate --diff`; `cargo hakari manage-deps --dry-run`; `cargo hakari verify` | Generated dependency-unification consistency | S; CI installs **cargo-hakari 0.9.38**; [CI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L201) |
| CI's `EmbarkStudios/cargo-deny-action`; `cargo audit` | Dependency policy / advisories | S; cargo-deny action configuration in [CI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L235), explicit audit command in [security workflow](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/security-check.yml#L23); not run |
| `mise run install`; `mise run dev:site`; `mise run dev:docs`; `mise run build:docs` | aube dependency install, website/docs dev, docs build | S; [mise.toml](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/mise.toml#L1), [site](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/site/package.json), [docs](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/docs-site/package.json); not run |
| `mise run deploy` | Build docs, copy to website, deploy Worker | S; [mise](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/mise.toml#L22), [site scripts](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/site/package.json); publishes externally, not run |
| `git diff --check` | Whitespace/conflict-marker check for the documentation patch | R; also rerun at final verification |

**Resolved contradiction — local gate versus CI.** `mise run verify` covers the
same basic formatter/clippy/test concerns, but CI uses nextest plus a separate
doctest pass and adds hakari, cargo-deny, and the ambient target-directory guard.
It is not an exact reproduction of CI. The main build matrix runs Linux and
Windows for PRs, and Linux/macOS/Windows for other events. Expensive steps are
skipped for a successful docs-only path classification, but still report job
results; merge groups force code checks. [CI](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L25),
[local gate](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/mise.toml#L31).

CI runs on pushes to `master`, pull requests, and merge groups. `CI Green`
aggregates build, clippy, fmt, hakari, deny, and target-directory jobs. The
security workflow separately runs `cargo audit` on pushes/PRs and guards an
advisory exception's exact dependency chain. Source comments describe required
checks, but **actual GitHub branch-protection/required-check settings are
[UNVERIFIED]** from this local checkout. A workflow existing does not prove
that GitHub blocks a merge. [CI trigger](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L3),
[aggregate job](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/ci.yml#L281),
[audit policy](https://github.com/getappz/agentflare/blob/a4e59d748a61dba653ff55e20052b09cb79376c9/.github/workflows/security-check.yml#L3).

There is no separate root JS typecheck, lint, or end-to-end task in the inspected
root/site/docs manifests. That is not a claim that the Rust suite has no
integration tests: [tests/](https://github.com/getappz/agentflare/tree/a4e59d748a61dba653ff55e20052b09cb79376c9/tests/) contains CLI integration tests, and unit
tests also live beside modules. No full test suite or external-agent end-to-end
run was performed for this documentation change.

---

Previous: [Overview](/docs/reference/architecture/overview/) | Next: [Internals](/docs/reference/architecture/internals/)

