use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum HookEvent {
    SessionStart {
        #[arg(long, value_enum)]
        agent: agent_registry::Agent,
    },
    PromptSubmit {
        #[arg(long, value_enum)]
        agent: agent_registry::Agent,
    },
    PreToolUse {
        #[arg(long, value_enum)]
        agent: agent_registry::Agent,
    },
    SessionEnd {
        #[arg(long, value_enum)]
        agent: agent_registry::Agent,
    },
}

#[derive(Args)]
pub struct HookArgs {
    #[command(subcommand)]
    pub event: HookEvent,
}

impl HookArgs {
    pub fn run(self) {
        match self.event {
            HookEvent::SessionStart { agent } => crate::hook::session_start(agent.as_str()),
            HookEvent::PromptSubmit { agent } => crate::hook::prompt_submit(agent.as_str()),
            HookEvent::PreToolUse { agent } => crate::hook::pre_tool_use(agent.as_str()),
            HookEvent::SessionEnd { agent } => crate::hook::session_end(agent.as_str()),
        }
    }
}
