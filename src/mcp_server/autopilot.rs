//! `autopilot` MCP tool: backing policy for the Claude plugin mod.

use super::*;

impl AgentflareMcp {
    pub fn autopilot_impl(&self, req: AutopilotRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "health" => Ok(Self::autopilot_health().to_string()),
            other => Err(ErrorData::invalid_params(
                format!(
                    "unknown autopilot action `{other}` (expected check|observe|briefing|checkpoint|next_work|health)"
                ),
                None,
            )),
        }
    }

    fn autopilot_health() -> serde_json::Value {
        let daemon = match crate::daemon::read_pid_from_file() {
            None => "down",
            Some(pid) => {
                let addr = crate::ipc::DaemonAddr::default_for_pid(pid);
                match crate::daemon_client::daemon_health_check(&addr) {
                    Ok(_) => "up",
                    Err(_) => "unknown",
                }
            }
        };
        let stale_binary = std::env::current_exe()
            .map(|exe| !crate::doctor::stale_hook_binaries(&exe, Some("claude")).is_empty())
            .unwrap_or(false);
        // lean-ctx: auth cooldown state not probed yet, upgrade when `next_work` needs it
        serde_json::json!({
            "daemon": daemon,
            "stale_binary": stale_binary,
            "auth": "ok",
            "mcp_version": env!("CARGO_PKG_VERSION"),
        })
    }
}
