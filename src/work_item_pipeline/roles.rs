// Role specifications for the SDD loop's headless dispatches (roadmap P0
// item 2 of docs/audits/2026-09-24-claude-code-feature-gap-analysis.md).
//
// Each role's identity and tool policy is compiled by
// `agent_registry::compile_role` into the target agent's own CLI flags
// (Claude Code: `--append-system-prompt`, `--disallowedTools`, ...), and
// folded into the user prompt for agents whose CLI has no confirmed
// equivalent. Before this, implementer, reviewer and judge were launched
// with the identical argv and differed only by prose in the user prompt — a
// reviewer could edit code, a judge could run the test suite.
//
// `include!`d into `work_item_pipeline.rs`, so `use` paths resolve there.

/// Which SDD role a dispatch plays this turn.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum SddRole {
    /// Implements (or fixes) the current task in the claimed worktree.
    Implementer,
    /// Review-only task's analyst. With `design_spec` the deliverable *is* a
    /// written document, so editing stays enabled; otherwise read-only.
    Analyst { design_spec: bool },
    /// Task reviewer, analysis reviewer, and re-reviewer: read-only, may run
    /// verification commands.
    Reviewer,
    /// Emits one JSON decision from the role replies it is shown: no edits,
    /// no shell.
    Judge,
}

const IMPLEMENTER_SYSTEM_PROMPT: &str = "You are the implementer in an agentflare SDD pipeline, working one task of a larger plan as a headless run. You are inside the task's claimed git worktree on its feature branch: commit there, never on the default branch. Run every build, test, or lint command in the foreground and wait for it before your final reply, and make that reply a short status the reviewer and judge can act on.";

const ANALYST_SYSTEM_PROMPT: &str = "You are the analyst in an agentflare SDD pipeline, working a review-only task as a headless run. File-editing tools are disabled for this session: read the code, run checks in the foreground, and report findings in your final reply.";

const DESIGN_SPEC_ANALYST_SYSTEM_PROMPT: &str = "You are the analyst in an agentflare SDD pipeline, working a design-spec task as a headless run. Your deliverable is a written design or analysis document, not application code: write the document, then report its path and a summary in your final reply.";

const REVIEWER_SYSTEM_PROMPT: &str = "You are the reviewer in an agentflare SDD pipeline, reviewing one task as a headless run. File-editing tools are disabled for this session: verify by reading the diff and by running checks in the foreground, never by changing code yourself. Your final reply must start with REVIEW_APPROVED or REVIEW_ISSUES:.";

const JUDGE_SYSTEM_PROMPT: &str = "You are the judge in an agentflare SDD pipeline. Editing and shell tools are disabled for this session: decide from the plan, the ledger, and the role reply you are shown. Your final reply must be exactly one JSON object and nothing else.";

/// JSON Schema for the judge's decision — the same shape
/// `build_judge_prompt` spells out in prose and `parse_judge_decision`
/// reads back. Sent as `--json-schema` on Claude Code when
/// `AGENTFLARE_JUDGE_JSON_SCHEMA` opts in (see `judge_json_schema`).
pub(crate) const JUDGE_DECISION_SCHEMA: &str = r#"{"type":"object","properties":{"action":{"type":"string","enum":["continue_task","fix_round","escalate","park_finding","rule_and_continue","insert_task","skip_task","advance_task","complete_pipeline"]},"rationale":{"type":"string"},"ledger_line":{"type":"string"},"task_model_tier":{"type":["string","null"],"enum":["mechanical","integration","architecture",null]}},"required":["action","rationale","ledger_line"],"additionalProperties":false}"#;

/// Env var that opts the judge into `--json-schema` (any value but `0`,
/// `false`, or empty). Off by default until the schema mode has been
/// confirmed against a live `agentflare work` run: with the schema on, the
/// typed decision arrives in the reply's `structured_output` field and is
/// preferred over the free-text reply, but the free-text parse stays as the
/// fallback either way.
pub(crate) const JUDGE_JSON_SCHEMA_ENV: &str = "AGENTFLARE_JUDGE_JSON_SCHEMA";

fn judge_json_schema() -> Option<String> {
    let opted_in = std::env::var(JUDGE_JSON_SCHEMA_ENV)
        .ok()
        .is_some_and(|v| !matches!(v.trim(), "" | "0" | "false" | "no" | "off"));
    opted_in.then(|| JUDGE_DECISION_SCHEMA.to_string())
}

/// Shell-family tools a judge has no business running. Mirrors the Bash
/// family `init::post_tool_use_matcher` names, minus the lowercase aliases
/// Claude Code never exposes as tool names.
const SHELL_TOOLS: &[&str] = &["Bash", "PowerShell", "mcp__lean-ctx__ctx_shell"];

fn mutating_tools() -> Vec<String> {
    crate::hook_redirect::MUTATING_TOOLS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

/// The agent-agnostic spec for `role`.
pub(crate) fn sdd_role_spec(role: SddRole) -> agent_registry::RoleSpec {
    let (name, system_prompt, disallowed_tools) = match role {
        SddRole::Implementer => ("implementer", IMPLEMENTER_SYSTEM_PROMPT, Vec::new()),
        SddRole::Analyst { design_spec: true } => {
            ("analyst", DESIGN_SPEC_ANALYST_SYSTEM_PROMPT, Vec::new())
        }
        SddRole::Analyst { design_spec: false } => {
            ("analyst", ANALYST_SYSTEM_PROMPT, mutating_tools())
        }
        SddRole::Reviewer => ("reviewer", REVIEWER_SYSTEM_PROMPT, mutating_tools()),
        SddRole::Judge => {
            let mut tools = mutating_tools();
            tools.extend(SHELL_TOOLS.iter().map(|s| (*s).to_string()));
            ("judge", JUDGE_SYSTEM_PROMPT, tools)
        }
    };
    agent_registry::RoleSpec {
        role: name.to_string(),
        system_prompt: Some(system_prompt.to_string()),
        disallowed_tools,
        json_schema: (role == SddRole::Judge)
            .then(judge_json_schema)
            .flatten(),
        ..agent_registry::RoleSpec::default()
    }
}

/// Compiles `role` for `agent_name`: the extra argv to append to the
/// dispatch (empty for agents without confirmed flags) and the prompt to
/// send, with the role's system prompt folded in whenever the argv can't
/// carry it. Unknown agent names get the prompt-fold path.
pub(crate) fn compile_sdd_role(
    agent_name: &str,
    role: SddRole,
    prompt: &str,
) -> (Vec<String>, String) {
    let spec = sdd_role_spec(role);
    let compiled = agent_registry::agent_by_name(agent_name)
        .map(|agent| agent_registry::compile_role(agent, &spec))
        .unwrap_or_default();
    let prompt = agent_registry::prompt_with_system_fallback(&compiled, &spec, prompt);
    (compiled.args, prompt)
}
