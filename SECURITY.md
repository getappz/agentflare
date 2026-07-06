# Security Policy

## Reporting a Vulnerability

Please report security issues privately, not as a public GitHub issue:

- **GitHub**: [Create a private security advisory](https://github.com/getappz/agentflare/security/advisories/new)
- **Response time**: best-effort acknowledgment within a few days (small, single-maintainer project)

## What agentflare Does (and Doesn't Do)

agentflare is a **local-only CLI**. `agentflare init --agent X` writes hook/rule
config files into your agent's own config (e.g. `~/.claude/settings.json`,
`.cursor/hooks.json`) and installs its two managed components — lean-ctx and
engram — via each tool's own package manager (`npm`, `go install`, `brew`).
`agentflare hook session-start|prompt-submit` runs at your agent's hook
call sites, reading/writing only files under `~/.agentflare/` and the config
paths listed in the README's "What Gets Created" section.

**Does:**
- Read/write its own state at `~/.agentflare/state.json`
- Write hook/rule config into the target agent's own settings files (only if absent — never overwrites)
- Shell out to `npm`, `go`, `brew`, `git` to install/check lean-ctx and engram

**Does NOT:**
- Make any network requests itself (no telemetry, no update check, no dependencies that touch the network — see `Cargo.toml`: `clap`, `serde`, `serde_json`, `dirs` only)
- Read or transmit file contents from your project
- Require elevated privileges

The main risk surface is the installer subprocess calls (`npm install -g
lean-ctx-bin`, `go install`, `brew install`) — agentflare only ever invokes
these with fixed, hardcoded package names, never with user-supplied input.

## Automated Checks

Every push and PR runs:
- `cargo test`
- `cargo audit` (dependency CVE scan)

## Windows Antivirus False Positives

Unsigned Rust binaries are commonly flagged by ML-based AV heuristics
(e.g. Microsoft Defender's `Wacatac.B!ml`) — this is a known false-positive
pattern, not specific to agentflare. That's why the default Windows install
path (`install.ps1`) builds from source on your own machine rather than
downloading a prebuilt `.exe`; the Scoop/cargo-install paths do ship a
prebuilt binary and could theoretically trip this heuristic. Verify any
release binary against the published `SHA256SUMS`, or build from source to
sidestep the question entirely.
