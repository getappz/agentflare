# agentflare rules

Static fallback for agents with no MCP support and no hook mechanism (e.g. Aider).
Everything else (Claude Code, Codex, Cursor, Windsurf, VS Code/Copilot, Cline,
Continue) gets a real integration via the `agentflare` CLI — see
https://github.com/getappz/agentflare. Use this file only if your tool isn't
one of those.

## Context compression — lean-ctx

Prefer [lean-ctx](https://github.com/yvgude/lean-ctx) tools over native equivalents:
read files with its compressed reader, run shell commands through its
compression wrapper, search code with its search/callgraph tools, orient in
unfamiliar code with its composed-context command before exploring.

```bash
npm install -g lean-ctx-bin && lean-ctx onboard
```

## Cross-session memory — engram

Use [engram](https://github.com/Gentleman-Programming/engram) MCP tools for
persistent cross-session memory: recall relevant context at session start,
store durable decisions/facts as you learn them, create a handoff before a
long session ends.

```bash
go install github.com/Gentleman-Programming/engram/cmd/engram@latest
engram mcp   # or whatever your tool's MCP config points at
```

## Web search

Use Exa for internet search when available — free-tier, no API key required.

## Git

Never add "Generated with Claude Code" or "Co-Authored-By: Claude" signatures.
Commit messages are the message only.
