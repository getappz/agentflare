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

Prefer [lean-ctx](https://github.com/yvgude/lean-ctx) tools over native equivalents:
read files with its compressed reader, run shell commands through its
compression wrapper, search code with its search/callgraph tools, orient in
unfamiliar code with its composed-context command before exploring.

```bash
npm install -g lean-ctx-bin && lean-ctx onboard
```

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
