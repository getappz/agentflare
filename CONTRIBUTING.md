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

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Repo structure

```text
agentflare/
├── src/
│   ├── main.rs              # clap CLI, dispatch
│   ├── init.rs              # `agentflare init --agent X` — runs every component
│   ├── hook.rs              # `agentflare hook session-start|prompt-submit --agent X`
│   ├── components.rs        # registry: each entry checks + fixes itself, host-aware
│   ├── paths.rs             # home-dir resolution
│   ├── state.rs             # ~/.agentflare/state.json — on/off flag for hooks
│   ├── rule_text.rs         # shared rule copy (Exa, git, lean-ctx, engram usage)
│   ├── engram_install.rs    # engram safe-install logic (go install/brew only)
│   ├── cost.rs              # cost tracking
│   ├── optimize.rs          # optimization logic
│   └── pricing.rs           # model pricing data
├── data/                    # static data files
├── install.sh               # Linux/macOS installer
├── install.ps1              # Windows installer
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

The CLA keeps agentflare MIT-licensed for everyone while allowing the maintainer
to relicense (e.g. for a hosted/commercial offering).

## License

agentflare is distributed under the MIT License; by contributing, your
contributions are licensed to the public under the same terms (see the [CLA](CLA.md)
for the full grant).
