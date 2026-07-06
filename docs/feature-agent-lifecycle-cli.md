# Feature: Unified Agent Lifecycle CLI (`agentflare agents`)

**Status:** draft  
**Priority:** high  
**Effort:** M (medium — ~2 weeks)

## Problem

Developers run multiple AI coding assistants (Claude Code, Codex, OpenCode,
Cursor, Gemini CLI, etc.). Each has its own install method, config location,
version scheme, and launch convention. Swapping between them, keeping versions
in sync, or auditing what's installed are manual, error-prone, and
fragmented.

## Solution

Add `agentflare agents <subcommand>` — a single CLI surface for the full
lifecycle of popular AI coding assistant CLIs:

```
agentflare agents install <agent>[@version]  # install or pin a version
agentflare agents list                       # show installed agents and version
agentflare agents update <agent>             # update to latest
agentflare agents uninstall <agent>          # remove
agentflare agents launch <agent> [--model X] # run the agent
agentflare agents doctor                     # health check across all agents
```

## Design

### Relationship to existing `Agent` infra

`src/main.rs` already has an `Agent` enum (`ClaudeCode, Codex, Cursor, Windsurf,
VscodeCopilot, Cline, Continue`) used by `agentflare init --agent <x>` to wire
hooks (`src/init.rs`). This feature consolidates onto that enum rather than
building a second, divergent identifier system — see Phase 0 below.

### Scope split: standalone CLI vs. editor-embedded

Full lifecycle management (install/update/uninstall/launch) only makes sense
for agents that ship as a **standalone CLI binary**. Editor-embedded
extensions (VS Code Copilot Chat, Cline) have no independent install/launch
path.

- **Lifecycle-managed (`Tier::Cli`):** claude-code, codex, opencode, cursor,
  windsurf, gemini-cli, github-copilot-cli, aider
- **Detection-only (`Tier::Extension`):** vscode-copilot, cline, continue

### Agent registry (`agent_registry.rs`)

Static catalog of the 11 agents above, each an `AgentSpec`:
- `id: Agent` — the existing enum, extended from 7 to 11 variants
- `display_name`, `tier` (`Cli` | `Extension`)
- `binary_names: &[&str]` — names to search on PATH
- `version_args: &[&str]` — usually `&["--version"]`

Config-directory presence and auth/credential detection are **out of scope
for this ticket** — see [#23](https://github.com/getappz/agentflare/issues/23),
which owns per-agent auth-file cataloging as part of the profile vault work.
Keeping that scoped there avoids this ticket depending on auth-file formats
that #23 is already tracking in detail.

### Detection strategy

| Tier | Signal | What it tells |
|------|--------|---------------|
| PATH | binary search over `binary_names` | Binary installed anywhere on system |
| Version | `<binary> --version`, regex `\d+\.\d+\.\d+` | Exact version, cached by `{binary_path, mtime, version}` in `~/.agentflare/state.json` |

Version cache rules (informed by `agents-cli`'s `agents.ts`):
- Cache key includes both `binary_path` and `mtime` — a reinstall to a
  different location invalidates correctly, not just a touched mtime.
- **Never persist a failed/unparseable version as a cached result.** A
  transient `--version` failure must not stick forever; always re-probe on
  the next call instead.

### Output format

`agents list` and `agents doctor` show **only installed agents** — not the
full registry with ✓/✗ columns. Status is binary for this ticket: `ready`
(version resolved) or `unknown` (binary found, version resolution failed).
Auth-based status (`needs auth`, `needs init`) is deferred to #23.

```
$ agentflare agents list

  AGENT           VERSION    STATUS
  claude-code     1.2.3      ready
  codex           0.128.0    ready
  gemini-cli      0.26.0     ready
  aider           -          unknown
```

`agents doctor` shows the same rows, plus for any `unknown` status a verbose
line with the binary path and the raw error/timeout reason.

Supports `--json` for scripting.

## Phases

### Phase 0 — Registry consolidation (1 day)
- Extend the existing `Agent` enum (7 → 11 variants: add `Opencode`,
  `GeminiCli`, `GithubCopilotCli`, `Aider`)
- Move `Agent` + new `AgentSpec`/`Tier`/`REGISTRY` into `agent_registry.rs`;
  `init.rs`/`hook.rs`/clap's `--agent` flag import from there unchanged
- Tag each entry `Tier::Cli` or `Tier::Extension`

### Phase 1 — Detect only (2 days)
- Ship `agentflare agents list` and `agentflare agents doctor` in a new
  `agents.rs`, both installed-only
- PATH scan + `--version` resolve, cached in `state.rs`'s existing
  `~/.agentflare/state.json` (extend `State` with `version_cache`)
- Hermetic tests only — stub binaries on a temp `PATH`, no dependency on
  what's actually installed on the dev/CI machine

### Phase 2 — Install/update/uninstall (4 days)
- `agentflare agents install <agent>[@version]` via npm/brew/curl as appropriate
- `agentflare agents update <agent>`
- `agentflare agents uninstall <agent>`
- Version pinning (`install codex@0.120.0`)
- Dry-run mode (`--dry-run`)

### Phase 3 — Launch (2 days)
- `agentflare agents launch <agent> [args...]`
- Passes through remaining args to the agent binary
- Pre-flight: checks install + auth, warns on missing
- Optional `--model`, `--mode` flags

### Phase 4 — Extend registry (1 day)
- Add remaining agents: cody, goose, amp, kiro, antigravity, grok, kimi,
  openclaw, droid
- Community agent definitions via config file

## Prior art

| Project | What it detects | Key takeaway |
|---------|----------------|--------------|
| [agents-cli](https://github.com/phnx-labs/agents-cli) | 14 agents, full lifecycle | Dual-path: version-managed + PATH, shims-aware, `isConfigured()` + `isCliInstalled()` |
| [coding_agent_account_manager](https://github.com/Dicklesworthstone/coding_agent_account_manager) | 5 agents, binary+auth+config | `caam detect` structured output, status categories |
| [agent-detector](https://github.com/dtcxzyw/agent-detector) | 70+ agents, runtime only | 3-tier detection (process/env/standard), no install management |
| [vibedetector](https://github.com/VacTube/vibedetector) | 9 agents, filesystem markers | Project-level file detection, JSON/compact output |
| [agentx](https://github.com/sageox/agentx) | 14 agents, full lifecycle | `IsInstalled()` with native detection, AGENT_ENV propagation |

## Related tickets

- [#23 — Auth Profile Vault & Account Switching](https://github.com/getappz/agentflare/issues/23)
  (CAAM-style: profile backup/restore, rotation, failover, isolation)

## Scope boundaries

In scope:
- CLI binary detection (PATH)
- Version resolution, cached
- Install/update/uninstall via package managers (Phase 2)
- Launch with pass-through args (Phase 3)
- JSON output for scripting

Out of scope (v1):
- Config directory presence + auth/credential detection (see [#23](https://github.com/getappz/agentflare/issues/23)) — `list`/`doctor` status is limited to `ready`/`unknown` (version resolved or not) until #23 lands
- Auth profile vault + account switching (see [#23](https://github.com/getappz/agentflare/issues/23))
- Multi-account rotation, cooldown, failover (see [#23](https://github.com/getappz/agentflare/issues/23))
- Profile isolation + shallow profiles (see [#23](https://github.com/getappz/agentflare/issues/23))
- MCP server management
- Skill/plugin installation
- Cloud agent dispatch (Rush, Codex cloud, etc.)
- Session/transcript parsing
- Cost tracking

## Success criteria

Phase 0:
- [ ] `Agent` enum and `agentflare init --agent <x>` behavior unchanged after
      the move into `agent_registry.rs`
- [ ] All 11 registry entries tagged `Tier::Cli` or `Tier::Extension`

Phase 1:
- [ ] `agents list` / `agents doctor` show only installed agents (no
      ✓/✗ rows for agents that aren't present)
- [ ] Detection works on macOS, Linux, Windows
- [ ] Version cache survives across runs, invalidates on binary path or
      mtime change, and never sticks on a failed/unparseable result
- [ ] No false positives for agents not actually installed
- [ ] `--json` output on both commands
