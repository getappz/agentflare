---
title: Getting started
description: Install agentflare and wire it into your AI coding agent.
---

## Install

The quickest path on Linux/macOS downloads a prebuilt binary. It verifies SHA-256
when `sha256sum` or `shasum` is available; otherwise it warns and continues. It builds
from source instead if run from inside a clone:

```bash
curl -fsSL https://agentflare.dev/install.sh | sh
```

Homebrew, Scoop (an unsigned Windows x86_64 binary), `cargo install --git`, and
build-from-source are supported too — see the
[README](https://github.com/getappz/agentflare#install-the-cli) for the full matrix and
the [CLI reference](/docs/cli/) once it's installed.

## Wire it into your agent

`agentflare init --agent <host>` — one command per tool, run once. It installs
immediately; a few secondary steps (rule refresh, gateway registration) still prompt for
confirmation unless you pass `-y`/`--yes` to run it unattended.

```bash
agentflare init --agent claude-code
agentflare init --agent cursor
agentflare init --agent windsurf
agentflare init --agent vscode-copilot
agentflare init --agent cline
agentflare init --agent continue
agentflare init --agent opencode
```

**Codex** needs only `agentflare init --agent codex`, which writes the rules, the hook
config and the MCP registration. Use init or the Codex plugin, not both: installing
both executes the same lifecycle hooks twice.

## What each run does

- Writes rule files, if not already present.
- Installs lean-ctx if it's missing (native `curl | sh` or the Homebrew installer).
- Wires MCP and, where supported, hooks into the host's configuration. Codex needs
  no plugin-loader step when using init.
- Is detection-first and idempotent: components that are already satisfied are skipped,
  and managed rules may be refreshed. See [Architecture](/docs/architecture/) for
  integration and first-use details.

## What's next

Read [Concepts](/docs/concepts/) next — it lays out the mental model (optimization vs.
coordination) that the rest of the docs assume. From there, the [CLI reference](/docs/cli/)
and [MCP tools reference](/docs/mcp-tools/) cover the full surface, and [Guides](/docs/guides/)
walk through specific tasks end to end.
