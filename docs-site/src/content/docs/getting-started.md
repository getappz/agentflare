---
title: Getting started
description: Install agentflare and wire it into your AI coding agent.
---

## Install

The quickest path on Linux/macOS downloads a prebuilt, checksum-verified binary (it
builds from source instead if run from inside a clone):

```bash
curl -fsSL https://raw.githubusercontent.com/getappz/agentflare/master/install.sh | sh
```

Homebrew, Scoop, `cargo install --git`, and build-from-source (the path for Windows,
since there's no signed prebuilt binary there yet) are all supported too — see the
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

**Codex** needs one extra step first, since its hook system only activates through its
own plugin loader:

```bash
codex plugin marketplace add getappz/agentflare
codex plugin install agentflare
```

Then run `agentflare init --agent codex` for the rules and lean-ctx setup — the hook
wiring itself comes from the plugin manifest, not `init`.

## What each run does

- Writes rule files, if not already present.
- Installs lean-ctx if it's missing (native `curl | sh` or the Homebrew installer).
- Wires hooks/MCP directly into the host's own config where it supports that — no plugin
  marketplace involved, Codex's plugin-loader step above being the one exception.
- Is detection-first and idempotent: components that are already satisfied are skipped,
  and nothing that's already there gets clobbered. Safe to re-run after an upgrade.

## What's next

Read [Concepts](/docs/concepts/) next — it lays out the mental model (optimization vs.
coordination) that the rest of the docs assume. From there, the [CLI reference](/docs/cli/)
and [MCP tools reference](/docs/mcp-tools/) cover the full surface, and [Guides](/docs/guides/)
walk through specific tasks end to end.
