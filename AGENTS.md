# leanstack rules

Read natively by Codex, Cursor, Windsurf, Copilot, Amp, Devin, and other agents
that honor `AGENTS.md`. No hooks here — these tools don't have a programmable
SessionStart/UserPromptSubmit mechanism, so this file just documents the rules
and the one manual install step. (Claude Code users: install the `leanstack`
plugin instead — it auto-detects and consent-gates the same stack via hooks.)

## Context compression — lean-ctx

Prefer [lean-ctx](https://github.com/yvgude/lean-ctx) tools over native equivalents:
- Read files with its compressed reader instead of a raw file read.
- Run shell commands through its compression wrapper instead of raw shell.
- Search code with its search/callgraph tools instead of grep.
- Orient in unfamiliar code with its composed-context command before exploring —
  one call instead of a manual search-read-search chain.

If not installed yet:

```bash
curl -fsSL https://leanctx.com/install.sh | sh   # or: npm install -g lean-ctx-bin
lean-ctx onboard                                  # wires MCP into this tool
```

## Web search

Use Exa for internet search when available. Skip built-in web-search/fetch tools —
Exa is free-tier, no API key required, and consistent across sessions.

## Git

- Never add "Generated with Claude Code" or "Co-Authored-By: Claude" signatures.
- Commit messages are the message only — nothing else appended.

## Code-writing discipline

Question whether new code needs to exist before writing it (YAGNI). Reach for
stdlib and native platform features before a dependency. Prefer the smallest
diff that actually solves the problem. ([Ponytail](https://github.com/DietrichGebert/ponytail)
codifies this further if your tool supports Claude Code-style plugins.)
