use serde_json::json;

pub enum AgentPlatform {
    Claude,
    Codex,
    Copilot,
    Fallback,
}

pub fn detect() -> AgentPlatform {
    if std::env::var("CLAUDE_CONFIG_DIR").is_ok() {
        AgentPlatform::Claude
    } else if std::env::var("COPILOT_PLUGIN_DATA").is_ok() {
        AgentPlatform::Copilot
    } else if std::env::var("PLUGIN_DATA").is_ok() {
        AgentPlatform::Codex
    } else {
        AgentPlatform::Fallback
    }
}

pub fn format_hook_output(event: &str, ctx: &str, platform: &AgentPlatform) -> String {
    match platform {
        AgentPlatform::Claude => {
            json!({
                "hookSpecificOutput": {
                    "hookEventName": event,
                    "additionalContext": ctx,
                }
            })
            .to_string()
        }
        AgentPlatform::Codex => {
            let sys_msg = if event == "SessionStart" {
                "PONYTAIL:FULL"
            } else {
                ""
            };
            let mut output = json!({
                "hookSpecificOutput": {
                    "hookEventName": event,
                    "additionalContext": ctx,
                }
            });
            if !sys_msg.is_empty() {
                output["systemMessage"] = json!(sys_msg);
            }
            output.to_string()
        }
        AgentPlatform::Copilot => {
            if event == "SessionStart" {
                json!({ "additionalContext": ctx }).to_string()
            } else {
                String::new()
            }
        }
        AgentPlatform::Fallback => ctx.to_string(),
    }
}
