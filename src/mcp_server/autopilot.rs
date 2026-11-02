//! `autopilot` MCP tool: backing policy for the Claude plugin mod.

use super::*;

impl AgentflareMcp {
    pub fn autopilot_impl(&self, req: AutopilotRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "health" => Ok(Self::autopilot_health().to_string()),
            "check" => Ok(Self::autopilot_check(&req).to_string()),
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

    /// Guard parity with the PreToolUse hook: same functions, same order,
    /// same reason strings. `cwd` is applied by absolutizing the tool's path
    /// (never `set_current_dir`).
    fn autopilot_check(req: &AutopilotRequest) -> serde_json::Value {
        use crate::hook_redirect::{ITEM_TOOL_NAME, MUTATING_TOOLS};
        let raw_name = req.tool_name.as_deref().unwrap_or_default();
        let (tool, input) =
            crate::hook_redirect::unwrap_gateway_call(raw_name, req.tool_input.as_ref());
        let is_item = tool == ITEM_TOOL_NAME || tool == "item";
        if !is_item && !MUTATING_TOOLS.contains(&tool.as_str()) {
            return serde_json::json!({ "decision": "allow" });
        }
        let input = Self::anchor_to_cwd(&tool, input, req.cwd.as_deref());
        let deny = |rule: &str, reason: String| serde_json::json!({ "decision": "deny", "rule": rule, "reason": reason });
        let hook_reason = |d: &serde_json::Value| {
            d["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        if let Some(d) = crate::hook_redirect::redirect_decision(&tool, input.as_ref()) {
            let reason = hook_reason(&d);
            let rule = if reason.contains("local data files") {
                "data_guard"
            } else if reason.contains("default branch") {
                "branch_guard"
            } else {
                "redirect"
            };
            return deny(rule, reason);
        }
        if let Some(d) = crate::hook_claim_guard::claim_guard_decision(&tool, input.as_ref()) {
            return deny("claim_guard", hook_reason(&d));
        }
        if is_item {
            let agent = req.agent.as_deref().unwrap_or("claude");
            let key = crate::optimize::scoped_session_key(
                agent,
                req.session_id.as_deref().unwrap_or_default(),
            );
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // Missing record = no fresh evidence, same as the hook's fresh entry.
            let record = crate::optimize::load_runtime()
                .sessions
                .remove(&key)
                .unwrap_or(crate::optimize::SessionRecord {
                    start_ts: now,
                    turn_count: 0,
                    recent_tool_calls: vec![],
                    last_verification: None,
                    last_review: None,
                    last_diagnosis: None,
                    verification_start_tree: None,
                });
            let task_type = crate::hook::resolve_item_task_type(&tool, input.as_ref());
            if let Some(reason) = crate::hook_completion_gate::completion_gate_denial(
                &record,
                now,
                &tool,
                input.as_ref(),
                task_type.as_deref(),
            ) {
                return deny("completion_gate", reason);
            }
        }
        serde_json::json!({ "decision": "allow" })
    }

    /// Resolve a relative target path against `cwd`; a mutating tool with no
    /// path targets `cwd` itself (the hook falls back to process cwd).
    fn anchor_to_cwd(
        tool: &str,
        input: Option<serde_json::Value>,
        cwd: Option<&str>,
    ) -> Option<serde_json::Value> {
        let (Some(cwd), Some(mut v)) = (cwd, input.clone()) else {
            return input;
        };
        let Some(obj) = v.as_object_mut() else {
            return input;
        };
        let key = ["file_path", "path", "filePath"]
            .into_iter()
            .find(|k| obj.get(*k).is_some_and(|x| x.is_string()));
        match key {
            Some(k) => {
                let p = obj[k].as_str().unwrap_or_default();
                if !std::path::Path::new(p).is_absolute() {
                    let joined = std::path::Path::new(cwd).join(p);
                    obj.insert(k.into(), joined.display().to_string().into());
                }
            }
            None if crate::hook_redirect::MUTATING_TOOLS.contains(&tool) => {
                // `.` makes the ancestor walk start at cwd itself.
                let here = std::path::Path::new(cwd).join(".");
                obj.insert("path".into(), here.display().to_string().into());
            }
            None => {}
        }
        Some(v)
    }
}
