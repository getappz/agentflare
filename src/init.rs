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

fn confirm_ponytail_migration(agent: &str, yes: bool) -> bool {
    let detected = match agent {
        "claude-code" | "cowork" => has_existing_ponytail_claude(),
        "cursor" | "cursor-cli" => has_existing_ponytail_cursor(),
        "opencode" => has_existing_ponytail_opencode(),
        _ => false,
    };

    if !detected {
        return true;
    }

    println!();
    println!("⚠ Existing ponytail plugin detected for {agent}.");
    println!("  agentflare has ponytail built-in — the npm plugin would conflict.");

    if !yes {
        print!("  Uninstall ponytail plugin? [Y/n] ");
        let mut input = String::new();
        let bytes_read = std::io::stdin().read_line(&mut input).ok();
        if bytes_read == Some(0) {
            println!("  Skipped. Re-run: agentflare init --agent {agent}");
            return false;
        }
        match input.trim().to_lowercase().as_str() {
            "y" | "yes" | "" => {}
            _ => {
                println!("  Skipped. Re-run: agentflare init --agent {agent}");
                return false;
            }
        }
    }

    match agent {
        "opencode" => {
            println!("  Running: opencode plugin uninstall ponytail@ponytail");
            match std::process::Command::new("opencode")
                .args(["plugin", "uninstall", "ponytail@ponytail"])
                .output()
            {
                Ok(out) => {
                    if out.status.success() {
                        println!("  ok    ponytail plugin uninstalled");
                    } else {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        println!("  fail  {stderr}");
                    }
                }
                Err(e) => println!("  fail  could not run opencode: {e}"),
            }
            true
        }
        "claude-code" | "cowork" => {
            println!("  Run '/plugin uninstall ponytail@ponytail' in a Claude Code session");
            true
        }
        _ => true,
    }
}

fn has_existing_ponytail_claude() -> bool {
    let path = home().join(".claude").join("settings.json");
    if let Ok(content) = fs::read_to_string(&path) {
        if let Ok(settings) = serde_json::from_str::<Value>(&content) {
            let hooks = settings.get("hooks");
            let has_ponytail = hooks
                .and_then(|h| h.get("SessionStart"))
                .map(|v| v.to_string().contains("ponytail"))
                .unwrap_or(false);
            let not_agentflare = hooks
                .and_then(|h| h.get("SessionStart"))
                .map(|v| !v.to_string().contains("agentflare"))
                .unwrap_or(true);
            return has_ponytail && not_agentflare;
        }
    }
    false
}

fn has_existing_ponytail_cursor() -> bool {
    let path = cwd().join(".cursor").join("hooks.json");
    if let Ok(content) = fs::read_to_string(&path) {
        has_ponytail_ref(&content) && !content.contains("agentflare")
    } else {
        false
    }
}

fn has_existing_ponytail_opencode() -> bool {
    let path = home().join(".config").join("opencode").join("opencode.jsonc");
    if let Ok(content) = fs::read_to_string(&path) {
        has_ponytail_ref(&content) && !content.contains("agentflare")
    } else {
        false
    }
}

fn has_ponytail_ref(content: &str) -> bool {
    content.to_lowercase().contains("ponytail")
}

pub fn run(agent: &str, yes: bool) {
    println!("agentflare init --agent {agent}\n");

    check_competing_plugins(agent);

    for c in get_components(agent) {
        if (c.check)() {
            println!("  skip  {} (already satisfied)", c.id);
        } else {
            println!("  {:<5} {}", (c.apply)(), c.id);
        }
    }

    match agent {
        "claude-code" => {
            wire_claude_code();
            if confirm_ponytail_migration(agent, yes) {
                wire_ponytail_hooks(agent);
            }
        }
        "cursor" => {
            wire_cursor();
            if confirm_ponytail_migration(agent, yes) {
                wire_ponytail_hooks(agent);
            }
        }
        "opencode" => {
            wire_opencode();
            if has_existing_ponytail_opencode() {
                println!();
                println!("  info  Ponytail plugin detected. Keep it — OpenCode uses");
                println!("        plugins for hooks, not config. Plugin + agentflare");
                println!("        work together (plugin handles hooks, agentflare provides");
                println!("        skill engine).");
            }
            wire_ponytail_opencode();
        }
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
    hooks_obj.entry("PreToolUse").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "hooks": [{ "type": "command", "command": format!("\"{bin}\" hook pre-tool-use --agent claude-code"), "timeout": 5 }]
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

fn wire_opencode() {
    let path = home().join(".config").join("opencode").join("opencode.jsonc");
    let rules_dir = home().join(".config").join("opencode").join("rules");
    let rule_files: &[&str] = &["exa.md", "git.md", "lean-ctx.md", "engram.md"];

    let mut config: Value = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    if !config.is_object() {
        config = json!({});
    }

    let instructions = config
        .as_object_mut()
        .unwrap()
        .entry("instructions")
        .or_insert_with(|| json!([]));

    let arr = match instructions.as_array_mut() {
        Some(a) => a,
        None => {
            *instructions = json!([]);
            instructions.as_array_mut().unwrap()
        }
    };

    let mut added = 0;
    for &file in rule_files {
        let rule_path = rules_dir.join(file);
        let path_str = rule_path.to_string_lossy().replace('\\', "/");
        let has_it = arr.iter().any(|v| {
            v.as_str()
                .map(|s| s.contains(file))
                .unwrap_or(false)
        });
        if !has_it && rule_path.exists() {
            arr.push(json!(path_str));
            added += 1;
        }
    }

    if added > 0 {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        match fs::write(&path, serde_json::to_string_pretty(&config).unwrap() + "\n") {
            Ok(_) => println!("  ok    opencode.jsonc instructions wired ({added} rule(s))"),
            Err(e) => println!("  fail  writing opencode.jsonc: {e}"),
        }
    } else if arr.is_empty() {
        println!("  info  opencode.jsonc — no rules to wire yet (run with rules present)");
    } else {
        println!("  skip  opencode.jsonc (already wired)");
    }
}

pub fn wire_ponytail_hooks(agent: &str) {
    match agent {
        "claude-code" | "cowork" => wire_ponytail_claude_code(),
        "cursor" | "cursor-cli" => wire_ponytail_cursor(),
        "opencode" => wire_ponytail_opencode(),
        _ => println!("  info  auto-wiring not supported for {agent}. Manual config required."),
    }
}

fn wire_ponytail_claude_code() {
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
        .map(|v| v.to_string().contains("ponytail"))
        .unwrap_or(false);
    if already_wired {
        println!("  skip  ponytail hooks already wired in ~/.claude/settings.json");
        return;
    }

    let obj = settings.as_object_mut().unwrap();
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks_obj = hooks.as_object_mut().unwrap();

    hooks_obj.entry("SessionStart").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "hooks": [{ "type": "command", "command": format!("\"{bin}\" ponytail hook session-start"), "timeout": 10 }]
    }));
    hooks_obj.entry("SubagentStart").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "hooks": [{ "type": "command", "command": format!("\"{bin}\" ponytail hook subagent-start"), "timeout": 5 }]
    }));
    hooks_obj.entry("UserPromptSubmit").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "hooks": [{ "type": "command", "command": format!("\"{bin}\" ponytail hook prompt-submit"), "timeout": 5 }]
    }));

    obj.insert("statusLine".to_string(), json!({
        "type": "command",
        "command": format!("\"{bin}\" ponytail hook statusline")
    }));

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(&path, serde_json::to_string_pretty(&settings).unwrap() + "\n") {
        Ok(_) => println!("  ok    ponytail hooks wired in ~/.claude/settings.json"),
        Err(e) => println!("  fail  writing ~/.claude/settings.json: {e}"),
    }
}

fn wire_ponytail_cursor() {
    let path = cwd().join(".cursor").join("hooks.json");
    let bin = agentflare_binary();

    if path.exists() {
        let existing = fs::read_to_string(&path).unwrap_or_default();
        if existing.contains("ponytail") {
            println!("  skip  ponytail hooks already wired in .cursor/hooks.json");
            return;
        }
    }

    let mut content: Value = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({ "version": 1, "hooks": {} }));
    if !content.is_object() {
        content = json!({ "version": 1, "hooks": {} });
    }

    let hooks = content.as_object_mut().unwrap()
        .entry("hooks").or_insert_with(|| json!({}));
    let hooks_obj = hooks.as_object_mut().unwrap();

    hooks_obj.entry("sessionStart").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "command": format!("\"{bin}\" ponytail hook session-start"),
        "type": "command",
        "timeout": 30
    }));
    hooks_obj.entry("beforeSubmitPrompt").or_insert_with(|| json!([])).as_array_mut().unwrap().push(json!({
        "command": format!("\"{bin}\" ponytail hook prompt-submit"),
        "type": "command",
        "timeout": 10
    }));

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(&path, serde_json::to_string_pretty(&content).unwrap() + "\n") {
        Ok(_) => println!("  ok    ponytail hooks wired in .cursor/hooks.json"),
        Err(e) => println!("  fail  writing .cursor/hooks.json: {e}"),
    }
}

fn wire_ponytail_opencode() {
    println!("  info  OpenCode uses plugin system for hooks, not config.");
    println!("        Keep @dietrichgebert/ponytail in plugin list.");
    println!("        The plugin's built-in hooks work alongside agentflare.");
}

fn check_competing_plugins(agent: &str) {
    let competitors: &[(&str, &[&str])] = &[
        ("lex-temple", &["lex-temple", "lex_temple"]),
        ("cc-md", &["cc-md", "cc_md"]),
    ];

    for (name, markers) in competitors {
        if let Some(where_found) = scan_agent_configs(agent, markers) {
            eprintln!(
                "[agentflare] warning: detected {name} in {where_found} — \
                 may conflict with agentflare. Set AGENTFLARE_IGNORE_CONFLICTS=true to suppress."
            );
        }
    }
}

fn scan_agent_configs(agent: &str, markers: &[&str]) -> Option<String> {
    let configs: &[&str] = match agent {
        "claude-code" | "cowork" => &[".claude/settings.json"],
        "cursor" | "cursor-cli" => &[".cursor/hooks.json"],
        "opencode" => &[".config/opencode/opencode.jsonc"],
        _ => return None,
    };

    if std::env::var("AGENTFLARE_IGNORE_CONFLICTS").is_ok() {
        return None;
    }

    for rel in configs {
        let path = home().join(rel);
        if let Ok(content) = std::fs::read_to_string(&path) {
            let lower = content.to_lowercase();
            if markers.iter().any(|m| lower.contains(m)) {
                return Some(rel.to_string());
            }
        }
    }
    None
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
            assert!(content.contains("PreToolUse"));
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

    #[test]
    fn wire_opencode_adds_instructions_to_fresh_config() {
        with_temp_home(|| {
            let config_path = home().join(".config").join("opencode").join("opencode.jsonc");
            let rules_dir = home().join(".config").join("opencode").join("rules");
            fs::create_dir_all(&rules_dir).unwrap();
            for &f in &["exa.md", "git.md", "lean-ctx.md", "engram.md"] {
                fs::write(rules_dir.join(f), format!("# {f}\n")).unwrap();
            }

            wire_opencode();
            let content = fs::read_to_string(&config_path).unwrap();
            let parsed: Value = serde_json::from_str(&content).unwrap();
            let instructions = parsed["instructions"].as_array().unwrap();
            assert!(instructions.len() >= 1);
            assert!(instructions.iter().any(|v| v.as_str().unwrap().contains("exa.md")));
        });
    }

    #[test]
    fn wire_opencode_is_idempotent() {
        with_temp_home(|| {
            let config_path = home().join(".config").join("opencode").join("opencode.jsonc");
            let rules_dir = home().join(".config").join("opencode").join("rules");
            fs::create_dir_all(&rules_dir).unwrap();
            fs::write(rules_dir.join("exa.md"), "# exa\n").unwrap();

            wire_opencode();
            let first = fs::read_to_string(&config_path).unwrap();
            wire_opencode();
            let second = fs::read_to_string(&config_path).unwrap();
            assert_eq!(first, second, "second run should not duplicate instructions");
        });
    }

    #[test]
    fn wire_opencode_preserves_existing_instructions() {
        with_temp_home(|| {
            let config_path = home().join(".config").join("opencode").join("opencode.jsonc");
            let rules_dir = home().join(".config").join("opencode").join("rules");
            fs::create_dir_all(config_path.parent().unwrap()).unwrap();
            fs::write(&config_path, r#"{"instructions": ["/some/existing/rule.md"], "mcp": {}}"#).unwrap();
            fs::create_dir_all(&rules_dir).unwrap();
            fs::write(rules_dir.join("exa.md"), "# exa\n").unwrap();

            wire_opencode();
            let content = fs::read_to_string(&config_path).unwrap();
            assert!(content.contains("/some/existing/rule.md"));
            let parsed: Value = serde_json::from_str(&content).unwrap();
            assert!(parsed["mcp"].is_object());
        });
    }

}
