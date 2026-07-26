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
@fallback: ctx_* missing from your tool list? It's behind the gateway — tool(action="search", query="ctx_read") then tool(action="execute", server="leanctx", tool=<name>, args={...})
@scope: every subagent"#;

pub const LEANCTX_SUPERSEDED: &[&str] = &[
    "Prefer lean-ctx over native tools: ctx_read > Read/cat, ctx_shell > Bash, ctx_search > Grep, ctx_glob > Glob. Orient with ctx_compose before exploring unfamiliar code — one call instead of a search-read-search chain. ctx_callgraph answers \"who calls X\", not grep. Same rule for every subagent.",
    "@use: lean-ctx over native tools — ctx_read>Read/cat, ctx_shell>Bash, ctx_search>Grep, ctx_glob>Glob, ctx_callgraph>grep for \"who calls X\"\n@when: unfamiliar code — ctx_compose FIRST, one call vs search→read→search chain\n@scope: every subagent",
];

pub const FLARE_DOCS: &str = r#"@use: mcp__flare__docs — search|get|list|refresh for third-party API docs (cached)
@ecosystems: rust (docs.rs, default) · npm (ecosystem="npm"; @scope/pkg auto-detected, untyped packages fall back to @types)
@when: writing/reviewing code against a library's API, or before citing its behavior from memory
@fallback: docs missing from your tool list? It's deferred — ToolSearch("select:mcp__flare__docs") first
@scope: every session + subagent"#;

/// The pre-rename rule body. Listed here so `agentflare init` replaces it on
/// existing installs rather than leaving a rule that points agents at a tool
/// name (`mcp__flare__flare_docs`) the server no longer exposes.
pub const FLARE_DOCS_SUPERSEDED: &[&str] = &[
    r#"@use: mcp__flare__flare_docs — search|get|list|refresh for Rust crate API docs (docs.rs-backed, cached)
@when: writing/reviewing code against a crate's API, or before citing a Rust library's behavior from memory
@fallback: flare_docs missing from your tool list? It's deferred — ToolSearch("select:mcp__flare__flare_docs") first
@scope: every session + subagent"#,
];

pub fn all() -> Vec<&'static str> {
    vec![EXA, GIT, LEANCTX, FLARE_DOCS]
}

/// opencode's own PreToolUse-equivalent: a local plugin dropped into
/// `~/.config/opencode/plugin/` (opencode auto-loads every file there, no
/// config registration needed). Shells out to the same `agentflare hook
/// pre-tool-use` classifier Claude Code's PreToolUse hook already uses, so
/// there's one branch-guard decision, not a JS reimplementation of it.
/// PostToolUseFailure judge prompt: decides whether a tool failure is
/// genuine friction worth a vent call. Explicitly separates "a guard blocked
/// exactly what it exists to block, cleanly" (not friction) from "a guard
/// blocked me in a confusing or undocumented way" (friction).
pub const VENT_JUDGE_PROMPT: &str = r#"A tool call just failed. Tool call details:
$ARGUMENTS

Judge whether this is genuine FRICTION worth logging via agentflare's vent tool (mcp__flare__vent), using the SAME broad definition its own classifier uses -- not just "wrong/missing tool":
- severity is high / this genuinely blocked progress, OR
- the situation involves a pattern like broke/broken/fails/failing/wrong/should/missing/can't/cannot/error/panic/crash/hang/stuck/fabricated, OR
- this is a repeat of a problem that already happened earlier this session.

This covers a wrong or missing tool, a fabricated assumption about the environment, an environment/tooling gap, a confusing or misleading error message, or a tool silently doing the wrong thing -- not just outright blocking failures.

NOT friction: an ordinary expected failure (a grep with no matches, a file legitimately absent during exploration, a normal retry, slow builds, a routine denied permission), OR a guard/hook that blocked exactly the thing it exists to block, with a clear reason and a discoverable next step (e.g. a branch-protection guard refusing a write to main, or a destructive-command guard refusing rm -rf) -- that guard did its job; it is not a bug.

Friction IS still the right call when a guard's block is confusing or its workaround is undocumented -- e.g. it names an alternative that doesn't exist, or leaves you guessing which action it actually wants.

If this is genuine friction: respond NOT ok, with a reason that nudges calling mcp__flare__vent with a concise, specific message (state the actual wrong tool/assumption/gap, not a vague complaint) and an appropriate severity.
If this is an ordinary failure or a guard working as intended: respond ok."#;

pub const OPENCODE_BRANCH_GUARD_JS: &str = r#"// opencode has no PreToolUse hook wired to agentflare's own PreToolUse guard
// (that's Claude-Code-only, ~/.claude/settings.json), and opencode's own
// permission config isn't branch-aware -- so write/edit/patch tools went
// through unchecked while HEAD is on master/main. Rather than duplicating
// branch-check logic here, this shells out to the same `agentflare hook
// pre-tool-use` entry point Claude Code's PreToolUse hook already uses --
// one classifier (src/hook_redirect.rs, backed by
// flare_git_core::branch::is_protected_branch), consulted from every
// harness. Fail-open if the agentflare binary is missing/errors, matching
// this repo's other guards' fail-open philosophy.
const GUARDED_TOOLS = new Set(["write", "edit", "patch", "apply_patch", "multiedit"])

export const BranchGuard = async (ctx) => {
  return {
    "tool.execute.before": async ({ tool, sessionID }, { args }) => {
      if (!GUARDED_TOOLS.has(tool)) return
      const payload = JSON.stringify({
        session_id: sessionID,
        tool_name: tool,
        tool_input: args ?? {},
      })
      let stdout
      try {
        stdout = await ctx.$`echo ${payload} | agentflare hook pre-tool-use`
          .cwd(ctx.directory)
          .quiet()
          .nothrow()
          .text()
      } catch {
        return // agentflare not on PATH, or the call itself failed -- fail open
      }
      let decision
      try {
        decision = JSON.parse(stdout)
      } catch {
        return // no JSON on stdout (e.g. a nudge-only run) -- nothing to deny
      }
      const out = decision?.hookSpecificOutput
      if (out?.permissionDecision === "deny") {
        throw new Error(`[branch-guard] ${out.permissionDecisionReason}`)
      }
    },
  }
}
"#;

/// Known-old wording for a rule file, keyed by its filename — empty for rules
/// that have never changed. Used to tell "this file still has text we shipped
/// before" (safe to offer a refresh) apart from "the user edited this" (leave
/// it alone).
pub fn superseded(filename: &str) -> &'static [&'static str] {
    match filename {
        "exa.md" => EXA_SUPERSEDED,
        "git.md" => GIT_SUPERSEDED,
        "lean-ctx.md" => LEANCTX_SUPERSEDED,
        "flare-docs.md" => FLARE_DOCS_SUPERSEDED,
        _ => &[],
    }
}
