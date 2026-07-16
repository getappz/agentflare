# agentflare rules

Static fallback for agents with no MCP support and no hook mechanism (e.g. Aider).
Everything else (Claude Code, Codex, Cursor, Windsurf, VS Code/Copilot, Cline,
Continue) gets a real integration via the `agentflare` CLI — see
https://github.com/getappz/agentflare. Use this file only if your tool isn't
one of those.

## Flare optimize module

agentflare ships a single consolidated compression/optimization module (`optimize`)
with four layers:

| Layer   | Command                       | What it does                          |
|---------|-------------------------------|---------------------------------------|
| output  | `agentflare optimize output`  | LLM-based prose compression (was caveman) |
| code    | `agentflare optimize code`    | Lazy senior dev code minimalism (was ponytail) |
| context | `agentflare optimize context` | Session transcript compaction via BM25 |
| runtime | (automatic via hooks)         | Session hygiene, model routing nudges  |

Legacy commands (`agentflare flare`, `agentflare caveman`, `agentflare ponytail`)
still work as backward-compatible aliases.

`agentflare optimize retrieve <id>` (and MCP `mcp__flare__optimize
action=retrieve`) recovers an original that the output layer compressed away
(CCR pattern). lean-ctx-compressed *reads* are instead recovered via
`ctx_read mode=raw` — agentflare does not re-cache them, because lean-ctx is
a separate sidecar not in agentflare's read path.

## Context compression — lean-ctx

**MANDATORY for code intelligence — do NOT use native Grep / Read-on-full-file /
shell `cat`/`grep`/`rg`/`find` to search or read code. Route ALL of it through
lean-ctx instead.** lean-ctx is in shadow mode: native file/search/shell calls
auto-route to `ctx_*` — but the rule below is the contract so agents without
shadow routing (Aider, plain shells) still comply.

- **Code search** → `ctx_search` (action=regex | semantic | symbol), NOT Grep/grep/rg.
  - exact symbol: `ctx_search(action=symbol, name=...)`
  - by meaning: `ctx_search(action=semantic, query=...)` (uses the on-demand
    dense index — no pre-build needed)
  - by pattern: `ctx_search(action=regex, pattern=...)`
- **Callers/callees** → `ctx_callgraph` (NOT grep for "who calls X").
- **Orient in unfamiliar code** → `ctx_compose` FIRST (one call vs
  search→read→search chain).
- **Read files** → `ctx_read` (compressed reader), prefer mode=anchored/full.
  Recover a compressed read verbatim via `ctx_read mode=raw`.
- **Shell** → `ctx_shell` (auto-compresses output).

Native `cat`/`grep`/`rg`/`find`/`Read`-whole-file are ONLY for: writing files,
git status/diff you will act on, and non-code text. Everything code-intelligence
goes through lean-ctx so the index stays the single source of truth.

```bash
npm install -g lean-ctx-bin && lean-ctx onboard
```

If `ctx_*` tools are genuinely unavailable in your runtime, fall back to the
native Grep/Read — but that is the exception, and you must say so.

## Cross-session memory

agentflare ships persistent memory in the binary itself — no separate
install. Recall relevant context at session start via the CLI (works even
without MCP support):

```bash
agentflare memory context
agentflare memory search "<query>"
```

Storing new memories (`memory_remember`) is exposed as an MCP tool; if your
tool has MCP support, prefer it there. Recall-only via the CLI otherwise.

## Web search

Use Exa for internet search when available — free-tier, no API key required.

## Git

Never add "Generated with Claude Code" or "Co-Authored-By: Claude" signatures.
Commit messages are the message only.
