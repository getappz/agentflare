// Shared rule copy — used by components.rs (per-host rule files) and could be
// reused by anything else that needs the same wording. One place to edit it.

pub const EXA: &str = "Use Exa MCP tools (web_search_exa, get_code_context_exa, company_research_exa) for internet search. Skip WebFetch/WebSearch/websearch-agent — Exa covers it for every session and subagent.";

pub const GIT: &str = "Commit messages are the message only: no \"Generated with Claude Code\", no Co-Authored-By trailer. `git commit -m \"...\"` format.";

pub const LEANCTX: &str = "Prefer lean-ctx over native tools: ctx_read > Read/cat, ctx_shell > Bash, ctx_search > Grep, ctx_glob > Glob. Orient with ctx_compose before exploring unfamiliar code — one call instead of a search-read-search chain. ctx_callgraph answers \"who calls X\", not grep. Same rule for every subagent.";

// Workflow-level, not tool-name-level: engram's exposed MCP tool names have
// shifted across versions, so pin the behavior, not the exact call names.
pub const ENGRAM: &str = "Use engram MCP tools for persistent cross-session memory: recall relevant prior context at the start of a session, store durable decisions/facts/preferences as you learn them (not every detail — the load-bearing ones), and create a session handoff before a long session ends or context gets tight. This is the single source of truth for cross-session memory — do not duplicate it into lean-ctx's own session/knowledge tools.";

pub fn all() -> Vec<&'static str> {
    vec![EXA, GIT, LEANCTX, ENGRAM]
}
