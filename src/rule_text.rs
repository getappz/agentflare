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

pub fn all() -> Vec<&'static str> {
    vec![EXA, GIT, LEANCTX]
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
        _ => &[],
    }
}
