use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum HookEvent {
    SessionStart {
        /// Omit to auto-detect the launching host (parent process walk + env fingerprints).
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
    PromptSubmit {
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
    PreToolUse {
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
    SessionEnd {
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
}

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
            HookEvent::SessionEnd { agent } => crate::hook::session_end(&resolve_agent(agent)),
        }
    }
}
