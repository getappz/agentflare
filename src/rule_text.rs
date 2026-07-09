// Shared rule copy — used by components.rs (per-host rule files) and could be
// reused by anything else that needs the same wording. One place to edit it.

// Tag vocabulary, shared across all rules below — kept small and consistent
// rather than inventing new tags per rule:
//   @use   primary tool/resource to reach for
//   @skip  what NOT to use instead
//   @when  trigger/timing condition
//   @how   mechanism, only when it's not obvious from @use
//   @rule  hard constraint / format requirement
//   @scope where this applies (session/subagent breadth)
pub const EXA: &str = r#"@use: Exa MCP tools — web_search_exa, get_code_context_exa, company_research_exa
@when: internet search
@skip: WebFetch, WebSearch, websearch-agent
@scope: every session + subagent"#;

pub const EXA_SUPERSEDED: &[&str] = &[
    "Use Exa MCP tools (web_search_exa, get_code_context_exa, company_research_exa) for internet search. Skip WebFetch/WebSearch/websearch-agent — Exa covers it for every session and subagent.",
];

pub const GIT: &str = r#"@rule: commit message = message only
@skip: "Generated with Claude Code" line, Co-Authored-By trailer
@how: git commit -m "...""#;

pub const GIT_SUPERSEDED: &[&str] = &[
    "Commit messages are the message only: no \"Generated with Claude Code\", no Co-Authored-By trailer. `git commit -m \"...\"` format.",
];

pub const LEANCTX: &str = r#"@use: lean-ctx over native tools — ctx_read>Read/cat, ctx_shell>Bash, ctx_search>Grep, ctx_glob>Glob, ctx_callgraph>grep for "who calls X"
@when: unfamiliar code — ctx_compose FIRST, one call vs search→read→search chain
@scope: every subagent"#;

pub const LEANCTX_SUPERSEDED: &[&str] = &[
    "Prefer lean-ctx over native tools: ctx_read > Read/cat, ctx_shell > Bash, ctx_search > Grep, ctx_glob > Glob. Orient with ctx_compose before exploring unfamiliar code — one call instead of a search-read-search chain. ctx_callgraph answers \"who calls X\", not grep. Same rule for every subagent.",
];

// Workflow-level, not tool-name-level: engram's exposed MCP tool names have
// shifted across versions, so pin the behavior, not the exact call names.
// Also don't assume a fixed access path: engram may be a native plugin
// (mcp__engram__*) or, when that's disabled to avoid duplicating agentflare's
// own gateway-registry, only reachable via gateway_search/gateway_execute.
// Absence of mcp__engram__* in ToolSearch does NOT mean engram is unavailable.
pub const ENGRAM: &str = r#"@use: engram for persistent cross-session memory
@when: session start -> recall prior context; learning -> store durable decisions/facts/prefs (load-bearing only); session end/context-tight -> create handoff
@how: direct mcp__engram__* or gateway-only — gateway_search(query) -> gateway_execute(server="engram", tool, args) — if native plugin disabled; try gateway_search before assuming unavailable
@rule: single source of truth for cross-session memory, don't duplicate into lean-ctx's session/knowledge tools
@rule: intent-first discovery applies to any gateway-fronted tool, not just engram"#;

// Prior wording of ENGRAM, kept so `init` can detect an on-disk rule file
// that still has old text (vs. one a user hand-edited) and offer to refresh
// it with consent, the same way `confirm_ponytail_migration` asks before
// touching an existing install. Two generations: the original (pre-gateway-
// discovery-fix) wording, and the fixed-but-uncompressed wording that shipped
// briefly in the same PR before the token-compression pass below.
pub const ENGRAM_SUPERSEDED: &[&str] = &[
    "Use engram MCP tools for persistent cross-session memory: recall relevant prior context at the start of a session, store durable decisions/facts/preferences as you learn them (not every detail — the load-bearing ones), and create a session handoff before a long session ends or context gets tight. This is the single source of truth for cross-session memory — do not duplicate it into lean-ctx's own session/knowledge tools.",
    "Use engram for persistent cross-session memory: recall relevant prior context at the start of a session, store durable decisions/facts/preferences as you learn them (not every detail — the load-bearing ones), and create a session handoff before a long session ends or context gets tight. This is the single source of truth for cross-session memory — do not duplicate it into lean-ctx's own session/knowledge tools. Its tools may be exposed directly as mcp__engram__* or only via the agentflare gateway (gateway_search(query) -> gateway_execute(server=\"engram\", tool, args)) if the native plugin is disabled — try gateway_search before concluding engram isn't available. This intent-first discovery applies to any gateway-fronted tool, not just engram.",
];

pub fn all() -> Vec<&'static str> {
    vec![EXA, GIT, LEANCTX, ENGRAM]
}

/// Known-old wording for a rule file, keyed by its filename — empty for rules
/// that have never changed. Used to tell "this file still has text we shipped
/// before" (safe to offer a refresh) apart from "the user edited this" (leave
/// it alone).
pub fn superseded(filename: &str) -> &'static [&'static str] {
    match filename {
        "exa.md" => EXA_SUPERSEDED,
        "git.md" => GIT_SUPERSEDED,
        "lean-ctx.md" => LEANCTX_SUPERSEDED,
        "engram.md" => ENGRAM_SUPERSEDED,
        _ => &[],
    }
}
