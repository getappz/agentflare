//! Background discovery loop: finds items labeled `ready-for-work` and
//! dispatches an `agentflare work` job for each one whose assignee is a
//! confirmed-autonomous agent (skips the rest with a comment).

/// Returns the matching `Agent` only if `agent_registry::autonomous_args`
/// confirms it has a headless permission-bypass flag — the same gate
/// `agentflare work` itself uses (`src/cli/work.rs`'s `run_work`).
pub(crate) fn resolve_confirmed_agent(assignee: &str) -> Option<agent_registry::Agent> {
    let agent = agent_registry::REGISTRY
        .iter()
        .find(|s| s.id.as_str() == assignee)
        .map(|s| s.id)?;
    agent_registry::autonomous_args(agent).map(|_| agent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_confirmed_agent_accepts_claude_code() {
        assert_eq!(
            resolve_confirmed_agent("claude-code"),
            Some(agent_registry::Agent::ClaudeCode)
        );
    }

    #[test]
    fn resolve_confirmed_agent_rejects_opencode() {
        assert_eq!(resolve_confirmed_agent("opencode"), None);
    }

    #[test]
    fn resolve_confirmed_agent_rejects_unknown_agent_string() {
        assert_eq!(resolve_confirmed_agent("not-a-real-agent"), None);
    }
}
