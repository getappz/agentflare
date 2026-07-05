// `leanstack init --agent X` — the one explicit, consent-is-the-invocation
// setup command. Runs every component (installs included — no separate
// confirm step, since running this command IS the consent), then wires the
// host's hook config directly where a hook mechanism exists and can be
// written without going through a plugin marketplace (Claude Code, Cursor).
// Codex's hook only activates through its plugin system, so that wiring
// lives in .codex-plugin/ instead, not here.
use crate::components::get_components;
use crate::paths::home;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_default()
}

fn leanstack_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "leanstack".to_string())
}

pub fn run(agent: &str) {
    println!("leanstack init --agent {agent}\n");

    for c in get_components(agent) {
        if (c.check)() {
            println!("  skip  {} (already satisfied)", c.id);
        } else {
            println!("  {:<5} {}", (c.apply)(), c.id);
        }
    }

    match agent {
        "claude-code" => wire_claude_code(),
        "cursor" => wire_cursor(),
        _ => {}
    }

    println!("\nDone. Restart {agent} if it was already running.");
}

fn wire_claude_code() {
    let path = home().join(".claude").join("settings.json");
    let mut settings: Value = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    if !settings.is_object() {
        settings = json!({});
    }
    let bin = leanstack_binary();

    let already_wired = settings
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .map(|v| v.to_string().contains("leanstack"))
        .unwrap_or(false);
    if already_wired {
        println!("  skip  ~/.claude/settings.json hooks (already wired)");
        return;
    }

    let obj = settings.as_object_mut().unwrap();
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks_obj = hooks.as_object_mut().unwrap();

    hooks_obj.entry("SessionStart").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "hooks": [{ "type": "command", "command": format!("\"{bin}\" hook session-start --agent claude-code"), "timeout": 10 }]
    }));
    hooks_obj.entry("UserPromptSubmit").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "hooks": [{ "type": "command", "command": format!("\"{bin}\" hook prompt-submit --agent claude-code"), "timeout": 5 }]
    }));

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(&path, serde_json::to_string_pretty(&settings).unwrap() + "\n") {
        Ok(_) => println!("  ok    ~/.claude/settings.json hooks wired"),
        Err(e) => println!("  fail  writing ~/.claude/settings.json: {e}"),
    }
}

fn wire_cursor() {
    let path = cwd().join(".cursor").join("hooks.json");
    if path.exists() {
        let existing = fs::read_to_string(&path).unwrap_or_default();
        if existing.contains("leanstack") {
            println!("  skip  .cursor/hooks.json (already wired)");
            return;
        }
        println!("  skip  .cursor/hooks.json (exists, not leanstack's — not overwriting)");
        return;
    }
    let bin = leanstack_binary();
    let content = json!({
        "version": 1,
        "hooks": {
            "sessionStart": [{ "command": format!("\"{bin}\" hook session-start --agent cursor"), "type": "command", "timeout": 30 }],
            "beforeSubmitPrompt": [{ "command": format!("\"{bin}\" hook prompt-submit --agent cursor"), "type": "command", "timeout": 10 }]
        }
    });
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(&path, serde_json::to_string_pretty(&content).unwrap() + "\n") {
        Ok(_) => println!("  ok    .cursor/hooks.json written"),
        Err(e) => println!("  fail  writing .cursor/hooks.json: {e}"),
    }
}
