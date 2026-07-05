<div align="center">

# leanstack

**lean-ctx powered token-saving stack. One install, detects what you have, adds only what's missing.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Claude Code Plugin](https://img.shields.io/badge/Claude_Code-Plugin-blue.svg)](https://github.com/getappz/leanstack)

</div>

---

## What this is

A fork of [espresso](https://github.com/mirkobozzetto/espresso) with
[lean-ctx](https://github.com/yvgude/lean-ctx) swapped in as the context-compression
backbone, replacing the two pieces that overlap it (RTK's CLI-output compression and
GitNexus's knowledge-graph code lookup — lean-ctx's `ctx_shell` and `ctx_compose`/
`ctx_callgraph` already cover both).

| What gets configured | Savings | How |
|---|---|---|
| **lean-ctx** | up to 99% on tool I/O | MCP server + `ctx_*` tool rules (context compression, code search, callgraphs) |
| **Global rules** | context savings | `~/.claude/rules/` — Exa search, clean git, lean-ctx usage |
| **Caveman ultra** | ~75% | Conversation compression (if Caveman plugin installed) |
| **Ponytail** | 47-77% on code tasks | YAGNI ladder — stdlib/native first, no speculative abstraction |

**Detection-first**: checks what's already configured and skips it. Never overwrites
existing rules or config.

**Consent-gated installs**: the first session only *lists* what's missing and the exact
command each would run. Nothing is installed until you type `/leanstack confirm`. Static
rule files (which just add usage guidance, not packages) are the one exception — those
write on first run same as espresso's always did, since they aren't installing anything.

---

## Install

```
/plugin marketplace add getappz/leanstack
/plugin install leanstack@leanstack
/reload-plugins
```

Restart Claude Code. First session prints what's missing and asks for
`/leanstack confirm` before installing anything.

---

## What Gets Created

```
~/.claude/rules/
├── exa.md          # Exa-only web search
├── git.md          # Clean commits (no signatures)
└── lean-ctx.md     # Prefer ctx_* tools over native Read/Grep/Bash/Glob

~/.config/caveman/config.json   # {"defaultMode": "ultra"} (if Caveman found)
~/.config/ponytail/config.json  # {"defaultMode": "ultra"}
~/.claude/.leanstack-rules-done       # rules written marker
~/.claude/.leanstack-confirmed        # install-confirmed marker
~/.claude/.leanstack-active           # mode flag
```

Nothing is created if it already exists.

---

## How It Works

Two hooks:

1. **SessionStart** — writes rule files (if missing), lists any pending package/plugin
   installs and how to confirm them, injects a short reminder into context.
2. **UserPromptSubmit** — handles `/leanstack confirm` (runs the actual installs),
   `/leanstack off`/`on`, and reinforces the lean-ctx/Exa/git rules every turn so they
   don't drift mid-session.

---

## Uninstall

```
/uninstall-plugin leanstack
```

```bash
rm ~/.claude/rules/exa.md ~/.claude/rules/git.md ~/.claude/rules/lean-ctx.md
rm ~/.claude/.leanstack-active ~/.claude/.leanstack-rules-done ~/.claude/.leanstack-confirmed
rm ~/.config/ponytail/config.json  # ~/.config/caveman/config.json if you want that reset too
```

Ponytail/Caveman plugins themselves stay installed (uninstall separately if wanted).

---

<div align="center">

MIT License — forked from [espresso](https://github.com/mirkobozzetto/espresso) by Mirko Bozzetto

</div>
