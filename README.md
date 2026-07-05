<div align="center">

# leanstack

**lean-ctx powered token-saving stack. One install, detects what you have, adds only what's missing.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Claude Code Plugin](https://img.shields.io/badge/Claude_Code-Plugin-blue.svg)](https://github.com/getappz/leanstack)

</div>

---

## What this is

A Claude Code plugin built around [lean-ctx](https://github.com/yvgude/lean-ctx) as the
context-compression backbone (95+ shell-output compression patterns, cached reads,
tree-sitter-backed code search and callgraphs), plus the two companion layers that
compress a *different* axis and so don't overlap it:

| What gets configured | Savings | How |
|---|---|---|
| **lean-ctx** | up to 99% on tool I/O | MCP server + `ctx_*` tool usage rules |
| **Global rules** | context savings | `~/.claude/rules/` — Exa search, clean git, lean-ctx usage |
| **Caveman ultra** | ~75% | Conversation compression (if Caveman plugin installed) |
| **Ponytail** | 47-77% on code tasks | YAGNI ladder — stdlib/native first, no speculative abstraction |

**Detection-first**: a component registry (`src/hooks/components.js`) checks what's
already configured and skips it. Never overwrites existing rules or config.

**Consent-gated installs**: the first session only *lists* what's missing and the exact
command each would run. Nothing that installs a package or plugin runs until you type
`/leanstack confirm`. Rule files are the one exception — those are just usage guidance,
not installs, so they write on first run.

---

## Cross-agent support

| Tool | Method | Auto-install |
|---|---|---|
| **Claude Code** | Plugin hooks (SessionStart + UserPromptSubmit) | Yes — detection, consent-gated install, per-turn reminders |
| **Codex, Cursor, Windsurf, Copilot, Amp, Devin, others** | `AGENTS.md` at project root | No — these tools have no programmable hook mechanism, so it's rules-only |

Claude Code gets the full stack because it's the only tool here with a verified hook
API this project actually exercises. Everyone else gets the same rules as static
context via `AGENTS.md`, with a manual `lean-ctx onboard` note — no auto-install
machinery to build against an unverified hook surface.

### Non-Claude-Code install

```bash
curl -sL https://raw.githubusercontent.com/getappz/leanstack/main/AGENTS.md > AGENTS.md
```

---

## Claude Code Install

```
/plugin marketplace add getappz/leanstack
/plugin install leanstack@leanstack
/reload-plugins
```

Restart Claude Code. First session prints what's missing and asks for
`/leanstack confirm` before installing anything.

---

## Architecture

```
src/hooks/
├── state.js          # single JSON state blob (~/.claude/leanstack/state.json)
├── components.js      # registry: each entry knows how to check + fix itself
├── session-start.js   # SessionStart hook — runs non-consent components,
│                       # lists consent-gated ones if not yet confirmed
└── prompt-submit.js    # UserPromptSubmit hook — /leanstack confirm|on|off,
                         # reinforces rules every turn
```

Adding a new managed component means adding one entry to `components.js` — neither
hook hardcodes per-tool logic.

---

## What Gets Created

```
~/.claude/rules/
├── exa.md          # Exa-only web search
├── git.md          # Clean commits (no signatures)
└── lean-ctx.md     # Prefer ctx_* tools over native Read/Grep/Bash/Glob

~/.config/caveman/config.json   # {"defaultMode": "ultra"} (if Caveman found)
~/.config/ponytail/config.json  # {"defaultMode": "ultra"}
~/.claude/leanstack/state.json  # active/confirmed state
```

Nothing is created if it already exists.

---

## Uninstall

```
/uninstall-plugin leanstack
```

```bash
rm ~/.claude/rules/exa.md ~/.claude/rules/git.md ~/.claude/rules/lean-ctx.md
rm -rf ~/.claude/leanstack
rm ~/.config/ponytail/config.json  # ~/.config/caveman/config.json too if you want that reset
```

Ponytail/Caveman plugins themselves stay installed (uninstall separately if wanted).

---

<div align="center">

MIT License

</div>
