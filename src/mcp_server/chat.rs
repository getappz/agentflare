//! Slash-command dispatch for the inbound chat channel (`crate::chat_channel`).
//! Each command is a thin wrapper over the same `*_inner` methods the `item`/
//! `project`/`pm` MCP tools call, so a chat command and its MCP-tool
//! equivalent can never drift apart -- this module only adds chat-friendly
//! text formatting on top of their JSON replies.

use super::*;

impl AgentflareMcp {
    /// Route a parsed `/command args` to its handler and return the reply
    /// text to send back to the chat. Never errors: any failure downstream
    /// (backend, missing project link, ...) is folded into the reply text
    /// instead, since there's no MCP client here to hand an `ErrorData` to.
    pub(crate) fn handle_chat_command(&self, command: &str, args: &str) -> String {
        match command {
            "status" => self.chat_status(),
            "project" => self.chat_project(),
            "new" => self.chat_new(args),
            "help" => Self::chat_help(),
            other => format!("Unknown command /{other}.\n\n{}", Self::chat_help()),
        }
    }

    fn chat_status(&self) -> String {
        let req = PmRequest {
            action: "standup".into(),
            ..Default::default()
        };
        match self.pm_inner(req).and_then(|json| {
            serde_json::from_str::<serde_json::Value>(&json)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))
        }) {
            Ok(v) => format_standup(&v),
            Err(e) => format!("status failed: {}", e.message),
        }
    }

    fn chat_project(&self) -> String {
        let req = ProjectRequest {
            action: "info".into(),
        };
        match self.project_inner(req).and_then(|json| {
            serde_json::from_str::<serde_json::Value>(&json)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))
        }) {
            Ok(v) => format!(
                "Project: {} ({})",
                v.get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?"),
                v.get("identifier")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("?"),
            ),
            Err(e) => format!("project lookup failed: {}", e.message),
        }
    }

    fn chat_new(&self, args: &str) -> String {
        let name = args.trim();
        if name.is_empty() {
            return "usage: /new <title>".to_string();
        }
        let req = ItemRequest {
            action: "create".into(),
            name: Some(name.to_string()),
            ..Default::default()
        };
        match self.item_inner(req).and_then(|json| {
            serde_json::from_str::<serde_json::Value>(&json)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))
        }) {
            Ok(v) => format!(
                "Created #{} \u{2014} {}",
                v.get("sequence_id")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default(),
                v.get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(name),
            ),
            Err(e) => format!("create failed: {}", e.message),
        }
    }

    fn chat_help() -> String {
        "Commands:\n\
         /status \u{2014} project standup (done / in progress / stuck)\n\
         /project \u{2014} the project this chat is linked to\n\
         /new <title> \u{2014} create a work item\n\
         /help \u{2014} this message\n\
         Anything else continues your agent session."
            .to_string()
    }
}

/// Render `item_standup`'s JSON reply as a short chat-friendly digest
/// instead of dumping the raw payload.
fn format_standup(v: &serde_json::Value) -> String {
    let done = v
        .get("done_count")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let in_progress = v
        .get("in_progress_count")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let stuck = v
        .get("stuck_count")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0);
    let mut out = format!("Done: {done}  In progress: {in_progress}  Stuck: {stuck}");
    if let Some(items) = v.get("stuck").and_then(serde_json::Value::as_array) {
        for item in items.iter().take(5) {
            let seq = item
                .get("sequence_id")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default();
            let name = item
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?");
            out.push_str(&format!("\n\u{26A0} #{seq} {name}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_standup_summarizes_counts_with_no_stuck_items() {
        let v = serde_json::json!({ "done_count": 3, "in_progress_count": 1, "stuck_count": 0 });
        assert_eq!(format_standup(&v), "Done: 3  In progress: 1  Stuck: 0");
    }

    #[test]
    fn format_standup_lists_up_to_five_stuck_items() {
        let v = serde_json::json!({
            "done_count": 0, "in_progress_count": 0, "stuck_count": 2,
            "stuck": [
                { "sequence_id": 42, "name": "Fix the thing" },
                { "sequence_id": 7, "name": "Other thing" },
            ]
        });
        let text = format_standup(&v);
        assert!(text.contains("#42 Fix the thing"));
        assert!(text.contains("#7 Other thing"));
    }

    #[test]
    fn handle_chat_command_help_lists_every_command() {
        let mcp = AgentflareMcp::default();
        let text = mcp.handle_chat_command("help", "");
        for cmd in ["/status", "/project", "/new", "/help"] {
            assert!(text.contains(cmd), "help text missing {cmd}");
        }
    }

    #[test]
    fn handle_chat_command_unknown_falls_back_to_help() {
        let mcp = AgentflareMcp::default();
        let text = mcp.handle_chat_command("bogus", "");
        assert!(text.starts_with("Unknown command /bogus."));
        assert!(text.contains("/help"));
    }

    #[test]
    fn handle_chat_command_new_without_a_title_shows_usage() {
        let mcp = AgentflareMcp::default();
        assert_eq!(mcp.handle_chat_command("new", "   "), "usage: /new <title>");
    }
}
