use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum HookEvent {
    /// Fires when an agent session starts; injects the agentflare context banner.
    SessionStart {
        /// Omit to auto-detect the launching host (parent process walk + env fingerprints).
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
    /// Fires when the user submits a prompt; extracts intent and adds routing context.
    PromptSubmit {
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
    /// Fires before a tool call executes; can block/redirect via a hook decision.
    PreToolUse {
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
    /// Fires after a tool call fails; classifies the failure and nudges the agent, rate-limited.
    PostToolFailure {
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
    /// Fires after a tool call succeeds; records verification evidence and
    /// surfaces the finishing-a-development-branch decision menu.
    PostToolUse {
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
    /// No-op — kept only so an old settings.json entry from a prior
    /// agentflare version doesn't start erroring after an upgrade. New
    /// installs never wire this (see init.rs).
    SessionEnd {
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
    /// DEPRECATED / unsupported no-op (see `hook::pre_compact` doc comment).
    /// Claude Code's PreCompact hook never consumed this hook's output;
    /// compaction-survival now lives in the lean-ctx sidecar. Kept only so
    /// existing settings.json wiring doesn't error after an upgrade.
    PreCompact {
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
}

/// Internal hook entry point invoked by an agent's lifecycle events. Not meant for direct use.
#[derive(Args)]
pub struct HookArgs {
    #[command(subcommand)]
    pub event: HookEvent,
}

/// Explicit `--agent` wins; otherwise auto-detect the host that invoked this
/// hook the same way the MCP server resolves its own identity (parent
/// process walk + agent env fingerprints, via the `agent-detector` crate).
fn resolve_agent(explicit: Option<agent_registry::Agent>) -> String {
    explicit
        .map(|a| a.as_str().to_string())
        .or_else(agent_detector::agent_name)
        .unwrap_or_else(|| "unknown".to_string())
}

impl HookArgs {
    pub fn run(self) {
        match self.event {
            HookEvent::SessionStart { agent } => crate::hook::session_start(&resolve_agent(agent)),
            HookEvent::PromptSubmit { agent } => crate::hook::prompt_submit(&resolve_agent(agent)),
            HookEvent::PreToolUse { agent } => crate::hook::pre_tool_use(&resolve_agent(agent)),
            HookEvent::PostToolFailure { agent } => {
                crate::hook::post_tool_failure(&resolve_agent(agent))
            }
            HookEvent::PostToolUse { agent } => crate::hook::post_tool_use(&resolve_agent(agent)),
            HookEvent::SessionEnd { agent } => crate::hook::session_end(&resolve_agent(agent)),
            HookEvent::PreCompact { agent } => crate::hook::pre_compact(&resolve_agent(agent)),
        }
    }
}
