# Contributing to agentflare

Thanks for your interest in agentflare — contributions are welcome.

## Quick start

### Prerequisites

- Rust (stable) via [rustup](https://rustup.rs/)
- Git

### Setup

```bash
git clone https://github.com/getappz/agentflare.git
cd agentflare

cargo build
cargo test
```

### Quality bar (required)

Run the local gate before pushing — it mirrors CI (`.github/workflows/ci.yml`) exactly:

```bash
mise run verify
```

Equivalent without mise:

```bash
cargo fmt --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings -A unsafe_code -A clippy::pedantic
cargo test --workspace
```

CI also runs `cargo-deny` (dependency licensing/advisories) and a target-dir guard; those aren't part of the local gate since they need network access or are CI-environment-specific.

## Repo structure

Same source of truth as the README's [Architecture](README.md#architecture) section —
keep the two in sync if you add or move a module.

```text
agentflare/
├── src/
│   ├── main.rs                  # clap CLI, dispatch
│   ├── cli/                     # one file per top-level subcommand (init, hook, optimize,
│   │                            # work, vent, auth, vault, git, review, skill, memory, ...) —
│   │                            # thin clap wiring; real logic lives in the modules it calls
│   ├── mcp_server/               # one file per mcp__flare__* MCP tool, + tests/
│   ├── optimize/                  # optimize output/code/context/retrieve/runtime — see README
│   ├── coaching/                   # session nudges: rule storage, CRUD, CLI presentation
│   ├── memory/                      # built-in persistent memory (SQLite + FTS5)
│   ├── github/                       # GitHub API client backing `flare_git`/`git`
│   ├── dashboard/                      # `agentflare serve` dashboard backend
│   ├── vent/                            # friction capture + auto-classification
│   ├── mentions/                         # @mention parsing/resolution
│   ├── ipc/                               # daemon transport (Unix socket / named pipe)
│   ├── dev_install/                        # `agentflare dev-install` — self-upgrade from a checkout
│   ├── update/                              # self-update check + binary swap
│   ├── ui/                                   # terminal UI helpers (cliclack-based)
│   ├── core/                                  # small shared primitives
│   ├── init.rs, hook.rs, components.rs          # component registry: init/hook wiring, host-aware
│   ├── paths.rs, state.rs, rule_text.rs           # home-dir resolution, on/off state, shared rule copy
│   ├── claims.rs, review.rs, artifacts.rs,
│   │   channels.rs                                 # coordination-layer core logic
│   ├── auth.rs, auth_crypt.rs, auth_db.rs,
│   │   auth_runner.rs, vault.rs                      # auth-profile vault + per-project secrets vault
│   ├── daemon.rs, daemon_autostart.rs,
│   │   daemon_client.rs                                # daemon lifecycle
│   ├── cost.rs, pricing.rs                               # cost/pricing tracking
│   └── mcp_server.rs, mcp_prompts.rs                       # MCP stdio server wiring
├── crates/                  # 17-member Cargo workspace (flare-code, flare-output,
│                            # agentflare-store, agentflare-backend, gateway-registry, ...)
├── dashboard/web/           # static frontend served by `agentflare serve`
├── data/                    # static data files (e.g. anthropic-pricing.json)
├── install.sh               # Linux/macOS installer
├── install.ps1              # Windows installer
├── .codex-plugin/           # Codex plugin manifest (its hooks require the plugin loader)
└── .github/                 # CI, templates, workflows
```

## Issues

- If your issue was closed but the problem persists, comment `/reopen` on it — as the original author, this reopens the issue automatically (GitHub itself doesn't let authors reopen maintainer-closed issues). Issues closed as *not planned* are a maintainer call and aren't reopened this way, but a comment is still welcome.

## Pull requests

- Keep PRs focused (one theme per PR)
- Include a short test plan (commands you ran)
- All tests must pass before merging

## Contributor License Agreement (CLA)

Before your first pull request can be merged, you need to sign our
[Contributor License Agreement](CLA.md). It is a one-time, automated step: the
CLA Assistant bot comments on your PR, and you sign by replying:

> I have read the CLA Document and I hereby sign the CLA

The CLA keeps agentflare openly licensed for everyone while allowing the maintainer
to relicense (e.g. for a hosted/commercial offering).

## License

agentflare is distributed under the Apache License 2.0 (see [LICENSE](LICENSE) and
[NOTICE](NOTICE) for third-party attributions); by contributing, your contributions
are licensed to the public under the same terms (see the [CLA](CLA.md) for the full
grant).
