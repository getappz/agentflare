//! Role specifications for headless dispatch.
//!
//! A `RoleSpec` says what a dispatched role *is* (its identity, as a system
//! prompt) and what it may *do* (tool policy, permission posture, effort),
//! plus how its reply is shaped (a JSON schema) and how its session is
//! named. It is agent-agnostic; [`compile_role`] turns it into the flags one
//! agent's CLI actually understands. Claude Code carries every field
//! natively (`--append-system-prompt`, `--allowedTools`, `--disallowedTools`,
//! `--permission-mode`, `--effort`, `--name`, `--json-schema`). Agents whose
//! CLI has no confirmed equivalent get an empty argv, and the caller folds
//! the identity into the user prompt via [`prompt_with_system_fallback`]
//! instead — the same posture the rest of this crate takes: only flags
//! confirmed against the agent's own `--help` are ever emitted, never
//! guessed.
//!
//! Background: docs/audits/2026-09-24-claude-code-feature-gap-analysis.md,
//! roadmap P0 item 2. Before this module, every SDD role (implementer,
//! reviewer, judge) was launched with the identical argv and shaped only by
//! prose in the user prompt.

use super::registry::Agent;

/// Permission posture for a role. The variants are Claude Code's
/// `--permission-mode` values, which double as the vocabulary other agents'
/// postures are mapped from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PermissionMode {
    /// Prompt on anything not pre-approved.
    Default,
    /// Read-only planning; no edits or commands.
    Plan,
    /// Auto-approve file edits, prompt on the rest.
    AcceptEdits,
    /// Never prompt; deny anything not pre-approved.
    DontAsk,
    /// Skip permission checks entirely (what `--dangerously-skip-permissions`
    /// enables at launch).
    BypassPermissions,
}

impl PermissionMode {
    /// The exact `--permission-mode` value Claude Code accepts.
    #[must_use]
    pub fn as_claude_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Plan => "plan",
            Self::AcceptEdits => "acceptEdits",
            Self::DontAsk => "dontAsk",
            Self::BypassPermissions => "bypassPermissions",
        }
    }
}

/// Reasoning effort for a role — Claude Code's `--effort` levels. The cheap
/// second axis next to model choice: a judge that only has to emit one
/// decision does not need the effort an implementer does.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    /// The exact `--effort` value Claude Code accepts.
    #[must_use]
    pub fn as_claude_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// What a headless role is and what it may do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleSpec {
    /// Short role name (`implementer`, `reviewer`, `judge`, ...), for logs
    /// and session naming.
    pub role: String,
    /// Identity and standing instructions, delivered as a system prompt
    /// where the agent supports one.
    pub system_prompt: Option<String>,
    /// Tools pre-approved for this role (Claude Code `--allowedTools`
    /// syntax). Empty means "no change".
    pub allowed_tools: Vec<String>,
    /// Tools removed from this role's pool (Claude Code `--disallowedTools`).
    /// Empty means "no change".
    pub disallowed_tools: Vec<String>,
    /// Permission posture. `None` leaves the agent's launch default in place.
    pub permission_mode: Option<PermissionMode>,
    /// Reasoning effort. `None` leaves the agent's default in place.
    pub effort: Option<Effort>,
    /// Session name (Claude Code `--name`), so a role's transcript is
    /// addressable by name instead of only by id.
    pub session_name: Option<String>,
    /// JSON Schema (serialized) the reply must conform to (Claude Code
    /// `--json-schema`). The structured result then arrives in the reply's
    /// `structured_output` field.
    pub json_schema: Option<String>,
}

/// The result of compiling a [`RoleSpec`] for one agent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompiledRole {
    /// Extra argv to append to the headless launch (after print-mode and
    /// autonomy flags, before or after `--resume` — order is irrelevant).
    pub args: Vec<String>,
    /// `true` when `args` already carry the system prompt, so the caller
    /// must not also prepend it to the user prompt.
    pub system_prompt_in_args: bool,
}

/// Compile `spec` into `agent`'s own CLI flags. Only confirmed flags are
/// emitted; everything else comes back as an empty [`CompiledRole`] and the
/// caller falls back to [`prompt_with_system_fallback`].
#[must_use]
pub fn compile_role(agent: Agent, spec: &RoleSpec) -> CompiledRole {
    match agent {
        // Confirmed via `claude --help` / code.claude.com/docs/en/cli-reference:
        // --append-system-prompt <text>, --allowedTools <list>,
        // --disallowedTools <list>, --permission-mode <mode>, --effort
        // <level>, --name <name>, --json-schema <schema>.
        Agent::ClaudeCode => compile_claude_code(spec),
        _ => CompiledRole::default(),
    }
}

fn push_flag(args: &mut Vec<String>, flag: &str, value: impl Into<String>) {
    args.push(flag.to_string());
    args.push(value.into());
}

fn compile_claude_code(spec: &RoleSpec) -> CompiledRole {
    let mut args = Vec::new();
    let mut system_prompt_in_args = false;
    if let Some(system_prompt) = spec
        .system_prompt
        .as_deref()
        .filter(|p| !p.trim().is_empty())
    {
        push_flag(&mut args, "--append-system-prompt", system_prompt);
        system_prompt_in_args = true;
    }
    if !spec.allowed_tools.is_empty() {
        push_flag(&mut args, "--allowedTools", spec.allowed_tools.join(","));
    }
    if !spec.disallowed_tools.is_empty() {
        push_flag(
            &mut args,
            "--disallowedTools",
            spec.disallowed_tools.join(","),
        );
    }
    if let Some(mode) = spec.permission_mode {
        push_flag(&mut args, "--permission-mode", mode.as_claude_str());
    }
    if let Some(effort) = spec.effort {
        push_flag(&mut args, "--effort", effort.as_claude_str());
    }
    if let Some(name) = spec
        .session_name
        .as_deref()
        .filter(|n| !n.trim().is_empty())
    {
        push_flag(&mut args, "--name", name);
    }
    if let Some(schema) = spec.json_schema.as_deref().filter(|s| !s.trim().is_empty()) {
        push_flag(&mut args, "--json-schema", schema);
    }
    CompiledRole {
        args,
        system_prompt_in_args,
    }
}

/// The prompt to actually send: `prompt` as-is when `compiled` already
/// carries the role's system prompt as a flag, otherwise the system prompt
/// folded in ahead of it. Never duplicates the identity.
#[must_use]
pub fn prompt_with_system_fallback(
    compiled: &CompiledRole,
    spec: &RoleSpec,
    prompt: &str,
) -> String {
    match spec.system_prompt.as_deref() {
        Some(system_prompt)
            if !compiled.system_prompt_in_args && !system_prompt.trim().is_empty() =>
        {
            format!("{system_prompt}\n\n{prompt}")
        }
        _ => prompt.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reviewer_spec() -> RoleSpec {
        RoleSpec {
            role: "reviewer".to_string(),
            system_prompt: Some("You are the reviewer.".to_string()),
            allowed_tools: vec![],
            disallowed_tools: vec!["Edit".to_string(), "Write".to_string()],
            permission_mode: Some(PermissionMode::DontAsk),
            effort: None,
            session_name: None,
            json_schema: None,
        }
    }

    #[test]
    fn claude_code_carries_every_field_as_its_own_flags() {
        let compiled = compile_role(Agent::ClaudeCode, &reviewer_spec());
        assert_eq!(
            compiled.args,
            vec![
                "--append-system-prompt",
                "You are the reviewer.",
                "--disallowedTools",
                "Edit,Write",
                "--permission-mode",
                "dontAsk",
            ]
        );
        assert!(compiled.system_prompt_in_args);
    }

    #[test]
    fn claude_code_emits_effort_name_and_schema_when_set() {
        let spec = RoleSpec {
            effort: Some(Effort::Low),
            session_name: Some("item-42:judge".to_string()),
            json_schema: Some(r#"{"type":"object"}"#.to_string()),
            ..RoleSpec::default()
        };
        let compiled = compile_role(Agent::ClaudeCode, &spec);
        assert_eq!(
            compiled.args,
            vec![
                "--effort",
                "low",
                "--name",
                "item-42:judge",
                "--json-schema",
                r#"{"type":"object"}"#,
            ]
        );
        assert!(!compiled.system_prompt_in_args);
    }

    #[test]
    fn claude_code_emits_allowed_tools_when_set() {
        let spec = RoleSpec {
            allowed_tools: vec!["Read".to_string(), "Grep".to_string()],
            ..RoleSpec::default()
        };
        let compiled = compile_role(Agent::ClaudeCode, &spec);
        assert_eq!(compiled.args, vec!["--allowedTools", "Read,Grep"]);
        assert!(!compiled.system_prompt_in_args);
    }

    #[test]
    fn claude_code_skips_blank_system_prompt_name_and_schema() {
        let spec = RoleSpec {
            system_prompt: Some("   ".to_string()),
            session_name: Some(String::new()),
            json_schema: Some(" ".to_string()),
            ..RoleSpec::default()
        };
        let compiled = compile_role(Agent::ClaudeCode, &spec);
        assert!(compiled.args.is_empty());
        assert!(!compiled.system_prompt_in_args);
    }

    #[test]
    fn unconfirmed_agents_get_no_flags() {
        for agent in [
            Agent::Codex,
            Agent::Cursor,
            Agent::Opencode,
            Agent::GeminiCli,
            Agent::Cline,
        ] {
            let compiled = compile_role(agent, &reviewer_spec());
            assert_eq!(compiled, CompiledRole::default(), "{agent:?}");
        }
    }

    #[test]
    fn fallback_prepends_system_prompt_only_when_not_carried_by_flags() {
        let spec = reviewer_spec();
        let via_flags = compile_role(Agent::ClaudeCode, &spec);
        assert_eq!(
            prompt_with_system_fallback(&via_flags, &spec, "Review this."),
            "Review this."
        );
        let via_prompt = compile_role(Agent::Codex, &spec);
        assert_eq!(
            prompt_with_system_fallback(&via_prompt, &spec, "Review this."),
            "You are the reviewer.\n\nReview this."
        );
    }

    #[test]
    fn fallback_is_a_no_op_without_a_system_prompt() {
        let spec = RoleSpec::default();
        let compiled = compile_role(Agent::Codex, &spec);
        assert_eq!(prompt_with_system_fallback(&compiled, &spec, "Go."), "Go.");
    }

    #[test]
    fn mode_and_effort_strings_match_claude_code() {
        assert_eq!(PermissionMode::Default.as_claude_str(), "default");
        assert_eq!(PermissionMode::Plan.as_claude_str(), "plan");
        assert_eq!(PermissionMode::AcceptEdits.as_claude_str(), "acceptEdits");
        assert_eq!(PermissionMode::DontAsk.as_claude_str(), "dontAsk");
        assert_eq!(
            PermissionMode::BypassPermissions.as_claude_str(),
            "bypassPermissions"
        );
        assert_eq!(Effort::Low.as_claude_str(), "low");
        assert_eq!(Effort::Medium.as_claude_str(), "medium");
        assert_eq!(Effort::High.as_claude_str(), "high");
        assert_eq!(Effort::XHigh.as_claude_str(), "xhigh");
        assert_eq!(Effort::Max.as_claude_str(), "max");
    }
}
