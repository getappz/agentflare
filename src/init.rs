// `agentflare init --agent X` — the one explicit, consent-is-the-invocation
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

fn agentflare_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "agentflare".to_string())
}

pub fn run(agent: &str) {
    println!("agentflare init --agent {agent}\n");

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
    let bin = agentflare_binary();

    let already_wired = settings
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .map(|v| v.to_string().contains("agentflare"))
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
        if existing.contains("agentflare") {
            println!("  skip  .cursor/hooks.json (already wired)");
            return;
        }
        println!("  skip  .cursor/hooks.json (exists, not agentflare's — not overwriting)");
        return;
    }
    let bin = agentflare_binary();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::{with_temp_cwd, with_temp_home};

    #[test]
    fn wire_claude_code_writes_hooks_to_fresh_settings() {
        with_temp_home(|| {
            wire_claude_code();
            let content = fs::read_to_string(home().join(".claude").join("settings.json")).unwrap();
            assert!(content.contains("agentflare"));
            assert!(content.contains("SessionStart"));
            assert!(content.contains("UserPromptSubmit"));
        });
    }

    #[test]
    fn wire_claude_code_is_idempotent() {
        with_temp_home(|| {
            let path = home().join(".claude").join("settings.json");
            wire_claude_code();
            let first = fs::read_to_string(&path).unwrap();
            wire_claude_code();
            let second = fs::read_to_string(&path).unwrap();
            assert_eq!(first, second, "second run should not duplicate hooks");
        });
    }

    #[test]
    fn wire_claude_code_preserves_existing_unrelated_settings() {
        with_temp_home(|| {
            let path = home().join(".claude").join("settings.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, r#"{"theme": "dark", "otherSetting": true}"#).unwrap();
            wire_claude_code();
            let content = fs::read_to_string(&path).unwrap();
            assert!(content.contains("dark"));
            assert!(content.contains("agentflare"));
        });
    }

    #[test]
    fn wire_claude_code_recovers_from_corrupt_settings_file() {
        with_temp_home(|| {
            let path = home().join(".claude").join("settings.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "not valid json{{{").unwrap();
            wire_claude_code();
            let content = fs::read_to_string(&path).unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
            assert!(parsed.is_object());
            assert!(content.contains("agentflare"));
        });
    }

    #[test]
    fn wire_cursor_writes_fresh_hooks_json() {
        with_temp_cwd(|| {
            wire_cursor();
            let content = fs::read_to_string(cwd().join(".cursor").join("hooks.json")).unwrap();
            assert!(content.contains("agentflare"));
            assert!(content.contains("sessionStart"));
        });
    }

    #[test]
    fn wire_cursor_skips_when_already_wired() {
        with_temp_cwd(|| {
            let path = cwd().join(".cursor").join("hooks.json");
            wire_cursor();
            let first = fs::read_to_string(&path).unwrap();
            wire_cursor();
            let second = fs::read_to_string(&path).unwrap();
            assert_eq!(first, second);
        });
    }

    #[test]
    fn wire_cursor_does_not_clobber_foreign_hooks_file() {
        with_temp_cwd(|| {
            let path = cwd().join(".cursor").join("hooks.json");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, r#"{"version": 1, "hooks": {}}"#).unwrap();
            wire_cursor();
            let content = fs::read_to_string(&path).unwrap();
            assert!(!content.contains("agentflare"));
        });
    }
}
