//! Job-scoped Claude Code configuration (audit roadmap P0 item 4).
//!
//! A headless `agentflare work` dispatch used to depend on the user's own
//! `~/.claude` state: the `flare` MCP server from `~/.claude.json` and the
//! agentflare hooks from `~/.claude/settings.json`, both written by
//! `agentflare init --agent claude-code`. On a host where `init` never ran
//! (a fresh CI runner, a daemon box, a teammate's machine) the job silently
//! ran without agentflare's tools, branch guard, or completion gate.
//!
//! [`job_scoped_args`] closes that gap with Claude Code's own per-run
//! configuration flags: `--mcp-config <file>` registers `flare` for this
//! run only, and `--settings <file>` adds the same hook wiring `init`
//! installs, as an additional settings layer. Each flag is emitted only
//! when the user-scope file lacks that piece, so a host that did run `init`
//! never gets the hooks twice. The files live under
//! `~/.agentflare/job-config/claude-code/`, never inside the worktree, so
//! nothing shows up as an untracked change the agent could commit.

use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

fn config_dir() -> PathBuf {
    crate::paths::agentflare_dir()
        .join("job-config")
        .join("claude-code")
}

/// True when user-scope `~/.claude/settings.json` already carries
/// agentflare's hook wiring (the `SessionStart` marker `init` writes).
fn user_hooks_wired() -> bool {
    let settings = crate::jsonc::read_jsonc(&crate::paths::claude_settings_path(), || Value::Null);
    settings
        .get("hooks")
        .and_then(|h| h.get("SessionStart"))
        .is_some_and(|v| v.to_string().contains("hook session-start"))
}

/// True when user-scope `~/.claude.json` already registers the `flare`
/// MCP server.
fn user_mcp_registered() -> bool {
    let root = crate::jsonc::read_jsonc(&crate::paths::claude_json_path(), || Value::Null);
    root.get("mcpServers")
        .and_then(|m| m.get("flare"))
        .is_some()
}

/// The `--settings` payload: agentflare's hook wiring (identical to what
/// `init` writes at user scope) plus the gateway tool allow-list.
pub(crate) fn settings_json(bin: &str) -> Value {
    let mut hooks = Map::new();
    crate::init::apply_claude_hook_specs(&mut hooks, bin);
    json!({
        "hooks": Value::Object(hooks),
        "permissions": { "allow": crate::components::GATEWAY_PERMISSIONS_ALLOW },
    })
}

/// The `--mcp-config` payload: the `flare` stdio server, same command
/// `claude mcp add flare -s user -- <bin> mcp` registers.
pub(crate) fn mcp_config_json(bin: &str) -> Value {
    json!({ "mcpServers": { "flare": { "command": bin, "args": ["mcp"] } } })
}

fn write_config(dir: &Path, name: &str, value: &Value) -> Option<String> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(name);
    let body = serde_json::to_vec_pretty(value).ok()?;
    std::fs::write(&path, body).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// Extra argv that makes a Claude Code job self-contained on this host.
/// Empty for other agents, and empty on a host where `agentflare init
/// --agent claude-code` already wired both the MCP server and the hooks.
pub(crate) fn job_scoped_args(agent: agent_registry::Agent) -> Vec<String> {
    if agent != agent_registry::Agent::ClaudeCode {
        return Vec::new();
    }
    let bin = crate::paths::agentflare_binary();
    let dir = config_dir();
    let mut args = Vec::new();
    if !user_mcp_registered()
        && let Some(path) = write_config(&dir, "mcp.json", &mcp_config_json(&bin))
    {
        args.push("--mcp-config".to_string());
        args.push(path);
    }
    if !user_hooks_wired()
        && let Some(path) = write_config(&dir, "settings.json", &settings_json(&bin))
    {
        args.push("--settings".to_string());
        args.push(path);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    }

    #[test]
    fn fresh_host_gets_both_files_and_both_flags() {
        with_temp_home(|| {
            let args = job_scoped_args(agent_registry::Agent::ClaudeCode);
            let mcp = flag_value(&args, "--mcp-config").expect("--mcp-config");
            let settings = flag_value(&args, "--settings").expect("--settings");

            let mcp: Value = serde_json::from_str(&std::fs::read_to_string(mcp).unwrap()).unwrap();
            assert_eq!(mcp["mcpServers"]["flare"]["args"], json!(["mcp"]));

            let settings: Value =
                serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap();
            for event in [
                "SessionStart",
                "UserPromptSubmit",
                "PreToolUse",
                "PostToolUse",
                "PostToolUseFailure",
                "Stop",
                "SessionEnd",
            ] {
                assert!(settings["hooks"][event].is_array(), "{event} hook wired");
            }
            assert!(
                settings["permissions"]["allow"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "mcp__flare__tool")
            );
        });
    }

    #[test]
    fn a_host_wired_by_init_gets_no_flags() {
        with_temp_home(|| {
            let claude_dir = crate::paths::claude_dir();
            std::fs::create_dir_all(&claude_dir).unwrap();
            std::fs::write(
                crate::paths::claude_settings_path(),
                r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"\"/x/agentflare\" hook session-start"}]}]}}"#,
            )
            .unwrap();
            std::fs::write(
                crate::paths::claude_json_path(),
                r#"{"mcpServers":{"flare":{"command":"/x/agentflare","args":["mcp"]}}}"#,
            )
            .unwrap();
            assert!(job_scoped_args(agent_registry::Agent::ClaudeCode).is_empty());
        });
    }

    #[test]
    fn only_the_missing_piece_is_supplied() {
        with_temp_home(|| {
            std::fs::write(
                crate::paths::claude_json_path(),
                r#"{"mcpServers":{"flare":{"command":"/x/agentflare","args":["mcp"]}}}"#,
            )
            .unwrap();
            let args = job_scoped_args(agent_registry::Agent::ClaudeCode);
            assert!(flag_value(&args, "--mcp-config").is_none(), "{args:?}");
            assert!(flag_value(&args, "--settings").is_some(), "{args:?}");
        });
    }

    #[test]
    fn other_agents_get_nothing() {
        with_temp_home(|| {
            assert!(job_scoped_args(agent_registry::Agent::Codex).is_empty());
            assert!(job_scoped_args(agent_registry::Agent::Opencode).is_empty());
        });
    }
}
