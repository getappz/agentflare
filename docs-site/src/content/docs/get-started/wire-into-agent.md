---
title: Wire into your agent
description: Register agentflare's hooks, MCP server, and rules with a supported coding agent.
---

`agentflare init --agent <host>` is a one-time, idempotent setup per host. It detects what
is already installed, writes the missing pieces, and merges host configuration rather than
overwriting it.

```bash
agentflare init --agent claude-code
agentflare init --agent codex
agentflare init --agent cursor
agentflare init --agent windsurf
agentflare init --agent vscode-copilot
agentflare init --agent cline
agentflare init --agent continue
agentflare init --agent opencode
```

## Host notes

| Host | What `init` wires |
|---|---|
| Claude Code | `~/.claude/settings.json` hooks; no marketplace step |
| Codex | `~/.codex/hooks.json` plus MCP registration |
| Cursor | `.cursor/hooks.json` directly |
| Windsurf, VS Code/Copilot, Cline, Continue, opencode | Rules, MCP registration, and supported hooks |

**Codex:** use either `init` or the packaged Codex plugin, not both. Installing both runs
the same lifecycle hooks twice. After setup, review and trust the hooks with `/hooks`.

A few optional steps (rule refresh, gateway registration) prompt for confirmation unless
you pass `-y`/`--yes`.

## What a run changes

- Writes rule files if they are not already present and refreshes managed rules.
- Installs [lean-ctx](https://github.com/yvgude/lean-ctx) if it is missing.
- Registers the MCP server and, where the host supports them, lifecycle hooks.
- Skips components that are already satisfied.

## Check the wiring

```bash
agentflare doctor
```

`doctor` reports missing or conflicting configuration per host and, with `--fix` where
supported, repairs it. `agentflare agents` lists detected agent installs.

## Next

- [Concepts](/docs/concepts/) — how the optimization and coordination layers differ.
- [Use flare-docs](/docs/guides/flare-docs/) — a first task that needs no extra setup.
- [CLI reference](/docs/reference/cli/) — the full command surface.
