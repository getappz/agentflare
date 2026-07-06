// `agentflare hook session-start --agent X` / `agentflare hook prompt-submit --agent X`
// The runtime handlers — invoked by whatever `agentflare init` (or, for
// Codex, the plugin manifest) wired into the target agent's hook config.
// No install/consent logic lives here: `init` is the explicit, one-shot
// consent; these just reinforce rules and report drift each session/turn.
use crate::components::get_components;
use crate::state;
use serde_json::json;
use std::io::Read;

pub fn session_start(agent: &str) {
    let components = get_components(agent);
    let mut lines = vec![];
    let mut pending = vec![];

    for c in &components {
        if (c.check)() {
            continue;
        }
        if c.needs_consent {
            pending.push(&c.describe);
        } else {
            lines.push((c.apply)());
        }
    }

    if !pending.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "agentflare: the following aren't set up yet — run `agentflare init --agent {agent}` to install them:"
        ));
        for d in pending {
            lines.push(format!("  - {d}"));
        }
    }

    lines.push(String::new());
    lines.push(
        "AGENTFLARE ACTIVE — lean-ctx/engram tools, Exa search, clean git commits. Off: /agentflare off."
            .to_string(),
    );

    println!("{}", lines.join("\n"));
}

fn extract_prompt(input: &str) -> String {
    serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|v| {
            v.get("prompt")
                .or_else(|| v.get("text"))
                .or_else(|| v.get("message"))
                .and_then(|p| p.as_str())
                .map(str::to_lowercase)
        })
        .unwrap_or_default()
}

pub fn prompt_submit(agent: &str) {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let prompt = extract_prompt(&input);
    let prompt = prompt.trim();

    let mut s = state::load();

    if prompt == "/agentflare off" || prompt == "/agentflare stop" {
        s.active = false;
        state::save(&s);
        return;
    }
    if prompt == "/agentflare on" {
        s.active = true;
        state::save(&s);
    }

    if !s.active {
        return;
    }

    let mut bits = vec![
        "AGENTFLARE ACTIVE.".to_string(),
        "Prefer lean-ctx ctx_* tools over native Read/Grep/Bash/Glob.".to_string(),
        "Exa is the only web search tool.".to_string(),
        "Clean git commits, no AI signature.".to_string(),
    ];
    let pending = get_components(agent)
        .iter()
        .any(|c| c.needs_consent && !(c.check)());
    if pending {
        bits.push(format!("Reminder: `agentflare init --agent {agent}` to finish setup."));
    }

    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": bits.join(" "),
        }
    });
    println!("{out}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_prompt_reads_prompt_key() {
        assert_eq!(extract_prompt(r#"{"prompt": "Hello World"}"#), "hello world");
    }

    #[test]
    fn extract_prompt_falls_back_to_text_key() {
        assert_eq!(extract_prompt(r#"{"text": "Foo"}"#), "foo");
    }

    #[test]
    fn extract_prompt_falls_back_to_message_key() {
        assert_eq!(extract_prompt(r#"{"message": "Bar"}"#), "bar");
    }

    #[test]
    fn extract_prompt_prefers_prompt_over_text_and_message() {
        assert_eq!(extract_prompt(r#"{"prompt": "A", "text": "B", "message": "C"}"#), "a");
    }

    #[test]
    fn extract_prompt_returns_empty_on_invalid_json() {
        assert_eq!(extract_prompt("not json"), "");
    }

    #[test]
    fn extract_prompt_returns_empty_when_no_known_key() {
        assert_eq!(extract_prompt(r#"{"other": "value"}"#), "");
    }
}
