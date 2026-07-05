// `leanstack hook session-start --agent X` / `leanstack hook prompt-submit --agent X`
// The runtime handlers — invoked by whatever `leanstack init` (or, for
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
            "leanstack: the following aren't set up yet — run `leanstack init --agent {agent}` to install them:"
        ));
        for d in pending {
            lines.push(format!("  - {d}"));
        }
    }

    lines.push(String::new());
    lines.push(
        "LEANSTACK ACTIVE — lean-ctx/engram tools, Exa search, clean git commits. Off: /leanstack off."
            .to_string(),
    );

    println!("{}", lines.join("\n"));
}

pub fn prompt_submit(agent: &str) {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let prompt: String = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| {
            v.get("prompt")
                .or_else(|| v.get("text"))
                .or_else(|| v.get("message"))
                .and_then(|p| p.as_str())
                .map(str::to_lowercase)
        })
        .unwrap_or_default();
    let prompt = prompt.trim();

    let mut s = state::load();

    if prompt == "/leanstack off" || prompt == "/leanstack stop" {
        s.active = false;
        state::save(&s);
        return;
    }
    if prompt == "/leanstack on" {
        s.active = true;
        state::save(&s);
    }

    if !s.active {
        return;
    }

    let mut bits = vec![
        "LEANSTACK ACTIVE.".to_string(),
        "Prefer lean-ctx ctx_* tools over native Read/Grep/Bash/Glob.".to_string(),
        "Exa is the only web search tool.".to_string(),
        "Clean git commits, no AI signature.".to_string(),
    ];
    let pending = get_components(agent)
        .iter()
        .any(|c| c.needs_consent && !(c.check)());
    if pending {
        bits.push(format!("Reminder: `leanstack init --agent {agent}` to finish setup."));
    }

    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": bits.join(" "),
        }
    });
    println!("{out}");
}
