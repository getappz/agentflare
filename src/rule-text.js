#!/usr/bin/env node
// Shared rule copy — used by the Claude Code/Codex/Cursor hooks
// (components.js) and by bin/setup.js for the hook-less tools. One place to
// edit the actual wording.
module.exports = {
  exa: 'Use Exa MCP tools (web_search_exa, get_code_context_exa, company_research_exa) for internet search. Skip WebFetch/WebSearch/websearch-agent — Exa covers it for every session and subagent.',
  git: 'Commit messages are the message only: no "Generated with Claude Code", no Co-Authored-By trailer. `git commit -m "..."` format.',
  leanctx: 'Prefer lean-ctx over native tools: ctx_read > Read/cat, ctx_shell > Bash, ctx_search > Grep, ctx_glob > Glob. Orient with ctx_compose before exploring unfamiliar code — one call instead of a search-read-search chain. ctx_callgraph answers "who calls X", not grep. Same rule for every subagent.',
};
