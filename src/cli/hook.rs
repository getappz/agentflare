use clap::{Args, Subcommand};
use std::time::Duration;

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
    /// Fires when an agent session ends; marks it ended in the live-session
    /// registry used for inter-agent messaging.
    SessionEnd {
        #[arg(long, value_enum)]
        agent: Option<agent_registry::Agent>,
    },
    /// Fires when the agent is about to stop; blocks the stop to deliver
    /// inter-agent messages that arrived during the turn.
    Stop {
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
/// process walk + agent env fingerprints, via `flare_process::agent`).
const AGENT_DETECT_HOOK_BUDGET: Duration = Duration::from_millis(500);

fn resolve_agent(explicit: Option<agent_registry::Agent>) -> String {
    if let Some(a) = explicit {
        return a.as_str().to_string();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(flare_process::agent_name());
    });
    match rx.recv_timeout(AGENT_DETECT_HOOK_BUDGET) {
        Ok(Some(name)) => name,
        _ => "unknown".to_string(),
    }
}

impl HookArgs {
    pub fn run(self) {
        let agent = resolve_agent(match self.event {
            HookEvent::SessionStart { agent } => agent,
            HookEvent::PromptSubmit { agent } => agent,
            HookEvent::PreToolUse { agent } => agent,
            HookEvent::PostToolFailure { agent } => agent,
            HookEvent::PostToolUse { agent } => agent,
            HookEvent::SessionEnd { agent } => agent,
            HookEvent::Stop { agent } => agent,
            HookEvent::PreCompact { agent } => agent,
        });
        let deadline = crate::hook_deadline::install(match self.event {
            HookEvent::SessionStart { .. } => crate::hook_deadline::Event::SessionStart,
            HookEvent::PromptSubmit { .. } => crate::hook_deadline::Event::PromptSubmit,
            HookEvent::PreToolUse { .. } => crate::hook_deadline::Event::PreToolUse,
            HookEvent::PostToolFailure { .. } => crate::hook_deadline::Event::PostToolFailure,
            HookEvent::PostToolUse { .. } => crate::hook_deadline::Event::PostToolUse,
            HookEvent::SessionEnd { .. } => crate::hook_deadline::Event::SessionEnd,
            HookEvent::Stop { .. } => crate::hook_deadline::Event::Stop,
            HookEvent::PreCompact { .. } => crate::hook_deadline::Event::PreCompact,
        });
        match self.event {
            HookEvent::SessionStart { .. } => crate::hook::session_start(&agent),
            HookEvent::PromptSubmit { .. } => crate::hook::prompt_submit(&agent),
            HookEvent::PreToolUse { .. } => crate::hook::pre_tool_use(&agent),
            HookEvent::PostToolFailure { .. } => crate::hook::post_tool_failure(&agent),
            HookEvent::PostToolUse { .. } => crate::hook::post_tool_use(&agent),
            HookEvent::SessionEnd { .. } => crate::hook::session_end(&agent),
            HookEvent::Stop { .. } => crate::hook_messages::stop(&agent),
            HookEvent::PreCompact { .. } => crate::hook::pre_compact(&agent),
        }
        drop(deadline);
    }
}
