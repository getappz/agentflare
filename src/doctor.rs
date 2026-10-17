// `agentflare doctor` — read-only setup/config health check. Distinct from
// `agentflare agents doctor` (agent BINARY/version detection) and
// `agentflare git doctor` (claim-worktree hygiene): this checks whether
// agentflare's OWN config (MCP registration, usage rules) is actually wired
// into each coding agent, reusing the exact `Component::check` closures
// `init` already runs, just without ever calling `apply`.
use crate::components::{get_components, rule_targets};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub host: String,
    pub component_id: &'static str,
    pub ok: bool,
    pub describe: String,
}

#[derive(Debug, Serialize)]
pub struct StaleRule {
    pub host: String,
    pub path: PathBuf,
}

/// An agentflare hook whose baked binary path is not the running executable.
#[derive(Debug, Serialize)]
pub struct StaleHook {
    pub host: String,
    pub path: PathBuf,
    pub command: String,
}

pub(crate) fn check_host(host: &str) -> Vec<CheckResult> {
    get_components(host)
        .into_iter()
        .map(|c| CheckResult {
            host: host.to_string(),
            component_id: c.id,
            ok: (c.check)(),
            describe: c.describe,
        })
        .collect()
}

/// Telegram chat-channel health, folded into the same `CheckResult` shape
/// `check_host` produces under a synthetic "telegram" host, so the existing
/// print/JSON layout renders it with no changes. Only runs when
/// `telegram_bot_token` is actually configured (opt-in, same as every other
/// Telegram code path) -- so `doctor` stays local-only/instant, and never
/// prompts for a vault passphrase, for the common case of nobody having set
/// this up. `token`'s `describe` shows the resolved `@bot_username` so it
/// can be eyeballed against whatever bot is actually being messaged --
/// the chat channel goes silently, permanently unreachable with zero error
/// anywhere when the configured token belongs to a different bot, and this
/// is the fast way to catch that instead of a long manual trace.
/// `send_test` (only attempted once `notify_chat` confirms a target
/// exists) actually calls `sendMessage` to that chat -- unlike `token` and
/// `notify_chat`, which only prove the plumbing is intact, this is the one
/// check that proves a message really lands in Telegram.
pub(crate) fn telegram_channel_checks() -> Vec<CheckResult> {
    // `list_secrets` only reads stored names, no vault unlock required --
    // unlike `get_secret` below, which needs a cached key or passphrase.
    // Gating presence on this (not `get_secret`) is what lets a genuinely
    // unconfigured setup skip silently while a *locked* vault that does
    // have a token configured still surfaces as a real failing check
    // instead of also vanishing indistinguishably.
    let Ok(names) = crate::vault::list_secrets() else {
        return Vec::new();
    };
    if !names.iter().any(|n| n == "telegram_bot_token") {
        return Vec::new();
    }
    let host = "telegram".to_string();
    let mut checks = match crate::channels::telegram_bot_identity() {
        Ok(username) => vec![CheckResult {
            host: host.clone(),
            component_id: "token",
            ok: true,
            describe: format!("bot {username} reachable"),
        }],
        Err(e) => {
            return vec![CheckResult {
                host,
                component_id: "token",
                ok: false,
                describe: e,
            }];
        }
    };
    // Fetched once (not per-check) -- the vault is already unsealed at this
    // point (the `token` check above required it), so this can't newly
    // fail on a lock; `chat_id` is `None` whenever it's simply unset.
    let chat_id = crate::vault::get_secret("telegram_notify_chat_id")
        .ok()
        .flatten();

    checks.push(CheckResult {
        host: host.clone(),
        component_id: "notify_chat",
        ok: chat_id.is_some(),
        describe: match &chat_id {
            // Resolves who that id actually belongs to (`getChat`) --
            // `telegram_notify_chat_id` can point at the wrong person
            // (stale, copy-pasted from an example, a former teammate's id)
            // exactly as silently as `telegram_bot_token` can point at the
            // wrong bot: outbound sends still succeed either way, so
            // nothing errors, and whoever is actually messaging the right
            // bot just never gets a reply. Falls back to the bare id if
            // `getChat` itself fails -- still `ok: true` since a working
            // chat id, even one the identity lookup couldn't resolve,
            // isn't itself the failure.
            Some(id) => match crate::channels::telegram_chat_identity(id) {
                Ok(who) => format!("notify chat configured ({who})"),
                Err(_) => "notify chat configured".to_string(),
            },
            None => "no telegram_notify_chat_id configured — set it with 'agentflare vault set telegram_notify_chat_id'".to_string(),
        },
    });

    // Only attempted once a chat id is actually on file -- `notify_chat`
    // above already reports the "nothing to send to" case, so this would
    // just be a redundant second failure otherwise.
    if let Some(chat_id) = chat_id {
        let send_result = crate::channels::send_message(
            crate::channels::Platform::Telegram,
            &chat_id,
            "agentflare doctor: channel check -- this bot can reach you.",
        );
        checks.push(CheckResult {
            host: host.clone(),
            component_id: "send_test",
            ok: send_result.is_ok(),
            describe: match send_result {
                Ok(()) => "test message delivered".to_string(),
                Err(e) => e,
            },
        });
    }
    // Live `flare-channels` transport state: registry presence, cumulative
    // counters, and realtime observers. Local-only, no network.
    checks.push(CheckResult {
        host,
        component_id: "transport",
        ok: true,
        describe: transport_describe(),
    });
    checks
}

/// Describe the `flare-channels` live transport for the `transport` doctor
/// check. Informational (`ok: true` always): a zero-traffic transport is a
/// healthy idle one — the supervisor's own poller predates the crate
/// transport, so counters only move once flows migrate onto it.
fn transport_describe() -> String {
    let Some(handle) = crate::channels::channel_handle("telegram") else {
        return "telegram transport not registered".to_string();
    };
    let status = handle.status();
    let observers = crate::channels::chat_bus().receiver_count();
    let mut describe = format!(
        "transport {} (sent {}, received {}, {} live observers)",
        if status.connected {
            "connected"
        } else {
            "idle"
        },
        status.messages_sent,
        status.messages_received,
        observers,
    );
    if let Some(err) = status.last_error {
        describe.push_str(&format!(" — last error: {err}"));
    }
    describe
}

/// First token of a hook command, respecting a leading double-quoted path.
fn command_binary(command: &str) -> Option<&str> {
    let command = command.trim();
    if let Some(rest) = command.strip_prefix('"') {
        let end = rest.find('"')?;
        let bin = &rest[..end];
        return (!bin.is_empty()).then_some(bin);
    }
    let token = command.split_whitespace().next()?;
    (!token.is_empty()).then_some(token)
}

fn is_agentflare_binary(bin: &str) -> bool {
    matches!(
        std::path::Path::new(bin)
            .file_name()
            .and_then(|n| n.to_str()),
        Some("agentflare" | "agentflare.exe")
    )
}

/// A bare `agentflare` resolves through PATH at hook time. A path (absolute
/// or with a directory component) was baked by init and can go stale.
fn is_baked_binary_path(bin: &str) -> bool {
    let path = std::path::Path::new(bin);
    path.is_absolute() || path.components().count() > 1
}

fn same_executable(hook: &std::path::Path, current: &std::path::Path) -> bool {
    if hook == current {
        return true;
    }
    match (std::fs::canonicalize(hook), std::fs::canonicalize(current)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// True when `command` invokes agentflare by a baked path that does not
/// resolve to `current_exe`.
pub(crate) fn hook_binary_is_stale(command: &str, current_exe: &std::path::Path) -> bool {
    let Some(bin) = command_binary(command) else {
        return false;
    };
    if !is_agentflare_binary(bin) || !is_baked_binary_path(bin) {
        return false;
    }
    !same_executable(std::path::Path::new(bin), current_exe)
}

fn collect_commands(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(cmd) = map.get("command").and_then(serde_json::Value::as_str) {
                out.push(cmd.to_string());
            }
            for child in map.values() {
                collect_commands(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_commands(child, out);
            }
        }
        _ => {}
    }
}

pub(crate) fn stale_hooks_in(
    host: &str,
    path: &std::path::Path,
    current_exe: &std::path::Path,
) -> Vec<StaleHook> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut commands = Vec::new();
    collect_commands(&value, &mut commands);
    commands
        .into_iter()
        .filter(|command| hook_binary_is_stale(command, current_exe))
        .map(|command| StaleHook {
            host: host.to_string(),
            path: path.to_path_buf(),
            command,
        })
        .collect()
}

fn hook_config_files() -> Vec<(&'static str, PathBuf)> {
    let mut files = vec![
        ("claude-code", crate::paths::claude_settings_path()),
        (
            "codex",
            crate::paths::home().join(".codex").join("hooks.json"),
        ),
    ];
    if let Ok(cwd) = std::env::current_dir() {
        files.push(("cursor", cwd.join(".cursor").join("hooks.json")));
    }
    files
}

/// Wired hook commands whose binary path is not `current_exe`.
pub(crate) fn stale_hook_binaries(current_exe: &std::path::Path) -> Vec<StaleHook> {
    hook_config_files()
        .into_iter()
        .flat_map(|(host, path)| stale_hooks_in(host, &path, current_exe))
        .collect()
}

pub(crate) fn stale_rules_for_host(host: &str) -> Vec<StaleRule> {
    rule_targets(host)
        .into_iter()
        .filter(|(path, current)| crate::init::is_stale_rule(path, current))
        .map(|(path, _)| StaleRule {
            host: host.to_string(),
            path,
        })
        .collect()
}

/// Hosts to check when `--agent` isn't given: every agent binary actually
/// detected on PATH — avoids reporting on config for tools the user doesn't
/// have installed. Presence-only (`detect_present`), not `detect_all`: this
/// only needs the ID list, and `detect_all` would additionally spawn a
/// `--version` subprocess per detected agent that doctor never looks at —
/// Node-wrapped CLIs make that spawn cost hundreds of ms each.
fn default_hosts() -> Vec<String> {
    agent_registry::detect::detect_present(agent_registry::REGISTRY)
        .into_iter()
        .map(str::to_string)
        .collect()
}

pub fn run(agent: Option<&str>, json: bool) {
    let hosts: Vec<String> = match agent {
        Some(a) => vec![a.to_string()],
        None => default_hosts(),
    };

    if hosts.is_empty() {
        eprintln!(
            "doctor: no coding agents detected on PATH; pass --agent <agent> to check one directly"
        );
        return;
    }

    let mut checks = Vec::new();
    let mut stale = Vec::new();
    for host in &hosts {
        checks.extend(check_host(host));
        stale.extend(stale_rules_for_host(host));
    }
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("agentflare"));
    let stale_hooks = stale_hook_binaries(&current_exe);

    // Not a coding-agent host like the others in `hosts` -- appended after
    // so the per-agent loop above (which calls `get_components`/
    // `rule_targets`, neither of which know about "telegram") never sees
    // it, while the print loop below picks it up the same way any other
    // host does.
    let mut display_hosts = hosts.clone();
    let channel_checks = telegram_channel_checks();
    if !channel_checks.is_empty() {
        display_hosts.push("telegram".to_string());
        checks.extend(channel_checks);
    }

    let mbx = crate::mbx::MbxStatus::collect();
    // Advisory only: never part of `healthy`, so an unset name cannot make
    // `doctor` exit non-zero.
    let machine_name = crate::github::bridge::config::read_machine_name();
    let healthy =
        checks.iter().all(|r| r.ok) && stale.is_empty() && stale_hooks.is_empty() && mbx.ok();

    if json {
        #[derive(Serialize)]
        struct Report<'a> {
            checks: &'a [CheckResult],
            stale_rules: &'a [StaleRule],
            stale_hooks: &'a [StaleHook],
            mbx: &'a crate::mbx::MbxStatus,
            machine_name: Option<&'a str>,
            healthy: bool,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Report {
                checks: &checks,
                stale_rules: &stale,
                stale_hooks: &stale_hooks,
                mbx: &mbx,
                machine_name: machine_name.as_deref(),
                healthy,
            })
            .unwrap()
        );
    } else {
        let total = checks.len();
        let passed = checks.iter().filter(|r| r.ok).count();

        println!("agentflare doctor\n");
        for host in &display_hosts {
            let host_checks: Vec<&CheckResult> =
                checks.iter().filter(|r| &r.host == host).collect();
            let host_stale: Vec<&StaleRule> = stale.iter().filter(|s| &s.host == host).collect();
            let host_hooks: Vec<&StaleHook> =
                stale_hooks.iter().filter(|s| &s.host == host).collect();
            let host_ok = host_checks.iter().filter(|r| r.ok).count();
            let host_total = host_checks.len();
            let host_healthy =
                host_ok == host_total && host_stale.is_empty() && host_hooks.is_empty();
            let mark = if host_healthy { "✓" } else { "✗" };

            println!("{mark} {host:16} {host_ok}/{host_total} ok");
            for r in host_checks.iter().filter(|r| !r.ok) {
                println!("    ✗ {:22} {}", r.component_id, r.describe);
            }
            for s in &host_stale {
                println!("    ⚠ stale rule: {}", s.path.display());
            }
            for h in &host_hooks {
                println!("    ⚠ stale hook binary: {}", h.command);
            }
        }
        for h in stale_hooks
            .iter()
            .filter(|h| !display_hosts.iter().any(|host| host == &h.host))
        {
            println!(
                "⚠ {} stale hook binary: {} ({})",
                h.host,
                h.command,
                h.path.display()
            );
        }

        println!("{}", mbx.format_text());
        match crate::memory::engine::unavailable_reason() {
            Some(why) => {
                println!("semantic search: unavailable ({why}); skill search is keyword-only")
            }
            None => println!("semantic search: ok"),
        }
        match &machine_name {
            Some(name) => println!("machine-name: {name}"),
            None => println!(
                "machine-name: not set -- PR labels show the raw instance id; run \
                 `agentflare init` or `agentflare config set machine-name <name>`"
            ),
        }
        let foreign = crate::github::pr_owner::foreign_pr_count(
            &crate::mcp_server::AgentflareMcp::default().worktree_repo_root(),
        );
        if foreign > 0 {
            println!("other-instance PRs: {foreign} open (inspect with `agentflare pr owner <n>`)");
        }
        println!();
        if healthy {
            println!("{passed}/{total} checks passed — all good.");
        } else {
            println!("{passed}/{total} checks passed — run `agentflare init` to fix.");
        }
    }

    if !healthy {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::rule_targets;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn check_host_reports_rules_missing_when_absent() {
        with_temp_home(|| {
            let results = check_host("claude-code");
            let rules = results
                .iter()
                .find(|r| r.component_id == "rules")
                .expect("claude-code should have a 'rules' component");
            assert!(!rules.ok);
        });
    }

    #[test]
    fn check_host_reports_rules_ok_once_targets_are_written() {
        with_temp_home(|| {
            for (path, content) in rule_targets("claude-code") {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, content).unwrap();
            }
            let results = check_host("claude-code");
            let rules = results
                .iter()
                .find(|r| r.component_id == "rules")
                .expect("claude-code should have a 'rules' component");
            assert!(rules.ok);
        });
    }

    #[test]
    fn stale_rules_for_host_detects_known_old_wording() {
        with_temp_home(|| {
            let targets = rule_targets("claude-code");
            for (path, content) in &targets {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, content).unwrap();
            }
            // Overwrite the exa.md target with its known-superseded wording.
            let (exa_path, _) = targets
                .iter()
                .find(|(p, _)| p.file_name().unwrap() == "exa.md")
                .unwrap();
            std::fs::write(exa_path, crate::rule_text::EXA_SUPERSEDED[0]).unwrap();

            let stale = stale_rules_for_host("claude-code");
            assert!(
                stale.iter().any(|s| s.path == *exa_path),
                "exa.md with superseded content should be flagged stale"
            );
        });
    }

    #[test]
    fn stale_rules_for_host_is_empty_when_content_is_current() {
        with_temp_home(|| {
            for (path, content) in rule_targets("claude-code") {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, content).unwrap();
            }
            assert!(stale_rules_for_host("claude-code").is_empty());
        });
    }

    #[test]
    fn stale_hooks_in_flags_a_baked_path_that_is_not_the_current_executable() {
        with_temp_home(|| {
            let path = crate::paths::claude_settings_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let current = std::env::current_exe().unwrap();
            std::fs::write(
                &path,
                serde_json::json!({
                    "hooks": {
                        "SessionStart": [{ "hooks": [
                            { "command": "\"/old/1.7.0/agentflare\" optimize code hook session-start" },
                            { "command": "\"agentflare\" hook session-start --agent claude-code" }
                        ] }],
                        "UserPromptSubmit": [{ "hooks": [
                            { "command": "unrelated-tool --flag" }
                        ] }]
                    },
                    "statusLine": {
                        "type": "command",
                        "command": format!("\"{}\" optimize code hook statusline", current.display())
                    }
                })
                .to_string(),
            )
            .unwrap();

            let stale = stale_hooks_in("claude-code", &path, &current);
            assert_eq!(stale.len(), 1, "{stale:?}");
            assert!(stale[0].command.contains("/old/1.7.0/agentflare"));

            std::fs::write(
                &path,
                serde_json::json!({
                    "hooks": {
                        "SessionStart": [{ "hooks": [{
                            "command": format!("\"{}\" optimize code hook session-start", current.display())
                        }] }]
                    }
                })
                .to_string(),
            )
            .unwrap();
            assert!(stale_hooks_in("claude-code", &path, &current).is_empty());
        });
    }

    #[cfg(unix)]
    #[test]
    fn hook_binary_is_stale_treats_a_symlink_to_the_current_executable_as_current() {
        with_temp_home(|| {
            let current = std::env::current_exe().unwrap();
            let link = crate::paths::home().join("agentflare");
            std::os::unix::fs::symlink(&current, &link).unwrap();
            let command = format!("\"{}\" optimize code hook prompt-submit", link.display());
            assert!(!hook_binary_is_stale(&command, &current));
        });
    }
}
