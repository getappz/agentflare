use serde_json::json;

use crate::detect;

pub enum AgentPlatform {
    Claude,
    Codex,
    Copilot,
    Fallback,
}

pub fn detect_platform() -> AgentPlatform {
    if let Some(result) = detect::detect() {
        match result.name.as_str() {
            "claude-code" | "cowork" => AgentPlatform::Claude,
            "codex" => AgentPlatform::Codex,
            "github-copilot" => AgentPlatform::Copilot,
            _ => AgentPlatform::Fallback,
        }
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
            let mut output = json!({ "additionalContext": ctx });
            if event == "SessionStart" {
                output["systemMessage"] = json!("PONYTAIL:FULL");
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
