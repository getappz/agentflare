use crate::paths::home;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

pub fn run(dry_run: bool, keep_config: bool, keep_binary: bool) {
    if dry_run {
        println!("--dry-run: would remove the following:");
    }

    if !keep_config {
        clean_claude_code(dry_run);
        clean_opencode(dry_run);
        clean_cursor(dry_run);
        clean_mcp_configs(dry_run);
        clean_ponytail_caveman(dry_run);
    }

    clean_state_dir(dry_run);

    if !keep_binary {
        clean_binary(dry_run);
    }

    if !dry_run {
        println!("done.");
    } else {
        println!("--dry-run complete. Run without --dry-run to actually remove.");
    }
}

fn remove_file(path: &PathBuf, dry_run: bool) {
    if path.exists() {
        if dry_run {
            println!("  rm {}", path.display());
        } else {
            let _ = fs::remove_file(path);
        }
    }
}

fn remove_dir(path: &PathBuf, dry_run: bool) {
    if path.exists() {
        if dry_run {
            println!("  rm -r {}", path.display());
        } else {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn clean_claude_code(dry_run: bool) {
    let rules = home().join(".claude").join("rules");
    for f in &["exa.md", "git.md", "lean-ctx.md"] {
        remove_file(&rules.join(f), dry_run);
    }

    let settings_path = home().join(".claude").join("settings.json");
    if settings_path.exists() {
        let content = fs::read_to_string(&settings_path).unwrap_or_default();
        if content.contains("agentflare")
            && let Ok(mut settings) = serde_json::from_str::<Value>(&content)
        {
            if let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
                for key in &["SessionStart", "UserPromptSubmit", "PreToolUse"] {
                    if let Some(arr) = hooks.get(*key).and_then(|v| v.as_array()) {
                        hooks[*key] = Value::Array(
                            arr.iter()
                                .filter(|entry| !entry.to_string().contains("agentflare"))
                                .cloned()
                                .collect(),
                        );
                    }
                }
                if hooks
                    .get("SessionStart")
                    .is_none_or(|v| v.as_array().is_none_or(|a| a.is_empty()))
                    && hooks
                        .get("UserPromptSubmit")
                        .is_none_or(|v| v.as_array().is_none_or(|a| a.is_empty()))
                    && hooks
                        .get("PreToolUse")
                        .is_none_or(|v| v.as_array().is_none_or(|a| a.is_empty()))
                {
                    hooks.remove("SessionStart");
                    hooks.remove("UserPromptSubmit");
                    hooks.remove("PreToolUse");
                }
                if hooks.is_empty() {
                    settings.as_object_mut().unwrap().remove("hooks");
                }
            }
            if dry_run {
                println!("  clean ~/.claude/settings.json (remove agentflare hooks)");
            } else {
                let _ = fs::write(
                    &settings_path,
                    serde_json::to_string_pretty(&settings).unwrap() + "\n",
                );
            }
        }
    }
}

fn clean_opencode(dry_run: bool) {
    let rules_dir = home().join(".config").join("opencode").join("rules");
    for f in &["exa.md", "git.md", "lean-ctx.md"] {
        remove_file(&rules_dir.join(f), dry_run);
    }

    let config_path = home()
        .join(".config")
        .join("opencode")
        .join("opencode.jsonc");
    if config_path.exists() {
        let content = fs::read_to_string(&config_path).unwrap_or_default();
        if (content.contains("agentflare")
            || content.contains("exa.md")
            || content.contains("engram.md"))
            && let Ok(mut config) = serde_json::from_str::<Value>(&content)
        {
            if let Some(instructions) = config
                .get_mut("instructions")
                .and_then(|v| v.as_array_mut())
            {
                instructions.retain(|v| {
                    let s = v.as_str().unwrap_or("");
                    !s.contains("exa.md")
                        && !s.contains("git.md")
                        && !s.contains("lean-ctx.md")
                        && !s.contains("engram.md")
                });
                if instructions.is_empty() {
                    config.as_object_mut().unwrap().remove("instructions");
                }
            }
            if dry_run {
                println!(
                    "  clean ~/.config/opencode/opencode.jsonc (remove agentflare instructions)"
                );
            } else {
                let _ = fs::write(
                    &config_path,
                    serde_json::to_string_pretty(&config).unwrap() + "\n",
                );
            }
        }
    }
}

fn clean_cursor(dry_run: bool) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let cursor_dir = cwd.join(".cursor");
    remove_file(&cursor_dir.join("rules").join("agentflare.mdc"), dry_run);

    let hooks_path = cursor_dir.join("hooks.json");
    if hooks_path.exists() {
        let content = fs::read_to_string(&hooks_path).unwrap_or_default();
        if content.contains("agentflare") && !content.contains(r#""version": 1"#) {
            remove_file(&hooks_path, dry_run);
        }
    }

    let mcp_path = home().join(".cursor").join("mcp.json");
    clean_mcp_entry(&mcp_path, dry_run);
}

fn clean_mcp_configs(dry_run: bool) {
    for mcp_file in &[
        home().join(".cursor").join("mcp.json"),
        home().join(".windsurf").join("mcp.json"),
        home().join(".vscode").join("mcp.json"),
    ] {
        clean_mcp_entry(mcp_file, dry_run);
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let engram_mcp = cwd.join(".continue").join("mcpServers").join("engram.json");
    let agentflare_mcp = cwd
        .join(".continue")
        .join("mcpServers")
        .join("agentflare.json");
    remove_file(&engram_mcp, dry_run);
    remove_file(&agentflare_mcp, dry_run);
}

fn clean_mcp_entry(path: &PathBuf, dry_run: bool) {
    if !path.exists() {
        return;
    }
    let content = fs::read_to_string(path).unwrap_or_default();
    // "flare" also matches "agentflare", covering the legacy entry name.
    if !content.contains("engram") && !content.contains("lean-ctx") && !content.contains("flare") {
        return;
    }
    if let Ok(mut config) = serde_json::from_str::<Value>(&content) {
        let key = if config.get("mcpServers").is_some() {
            "mcpServers"
        } else if config.get("mcp").is_some() {
            "mcp"
        } else {
            return;
        };
        if let Some(mcp) = config.get_mut(key).and_then(|v| v.as_object_mut()) {
            mcp.remove("engram");
            mcp.remove("lean-ctx");
            mcp.remove("flare");
            mcp.remove("agentflare");
            if mcp.is_empty() {
                config.as_object_mut().unwrap().remove(key);
            }
        }
        if dry_run {
            println!("  clean {} (remove agentflare MCP entries)", path.display());
        } else {
            let _ = fs::write(path, serde_json::to_string_pretty(&config).unwrap() + "\n");
        }
    }
}

fn clean_ponytail_caveman(dry_run: bool) {
    remove_file(
        &home().join(".config").join("ponytail").join("config.json"),
        dry_run,
    );
    remove_file(
        &home().join(".config").join("caveman").join("config.json"),
        dry_run,
    );
}

fn clean_state_dir(dry_run: bool) {
    let state = home().join(".agentflare");
    remove_dir(&state, dry_run);
}

fn clean_binary(dry_run: bool) {
    let install_dir = std::env::var("AGENTFLARE_INSTALL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".local").join("bin"));

    let current = std::env::current_exe().ok();
    if let Some(ref current) = current
        && current.to_string_lossy().contains("/target/")
    {
        if !dry_run {
            println!(
                "  skip binary (running from dev build at {})",
                current.display()
            );
        }
        return;
    }

    let binary_name = if cfg!(windows) {
        "agentflare.exe"
    } else {
        "agentflare"
    };
    let locations = &[
        install_dir.join(binary_name),
        PathBuf::from("/usr/local/bin").join(binary_name),
    ];

    for loc in locations {
        if loc.exists() {
            remove_file(loc, dry_run);
        }
    }
}
