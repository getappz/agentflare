// Component registry: each entry knows how to check itself and, if needed,
// fix itself. `init` runs every entry; `hook session-start` only runs the
// non-consent ones (rules/mode-pinning) since installing packages happens
// only via the explicit `init` command, never from an auto-firing hook.
use crate::engram_install;
use crate::paths::home;
use crate::rule_text;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub struct Component {
    pub id: &'static str,
    pub needs_consent: bool,
    pub describe: String,
    pub check: Box<dyn Fn() -> bool>,
    pub apply: Box<dyn Fn() -> String>,
}

// engram's own `engram setup <name>` covers these hosts natively (verified
// via `engram --help`). Cline/Continue aren't in that list, so they get a
// manual MCP config entry instead — same shape as lean-ctx's fallback.
const ENGRAM_NATIVE_HOSTS: &[&str] = &["codex", "cursor", "windsurf", "vscode-copilot"];
const ENGRAM_MCP_ARGS: &[&str] = &["mcp", "--tools=agent"];

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_default()
}

fn run_ok(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn claude_settings() -> Value {
    fs::read_to_string(home().join(".claude").join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

/// User-scope `claude mcp add` registrations live in `~/.claude.json`, a
/// separate file from `~/.claude/settings.json`.
fn claude_json() -> Value {
    fs::read_to_string(home().join(".claude.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

fn json_at(path: &PathBuf) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

fn plugin_enabled(settings: &Value, key: &str) -> bool {
    settings
        .get("enabledPlugins")
        .and_then(|p| p.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn write_pinned_mode(path: &PathBuf) -> bool {
    let current: Option<String> = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("defaultMode").and_then(|m| m.as_str()).map(String::from));
    if current.is_some() {
        return false;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, "{\"defaultMode\": \"ultra\"}\n").is_ok()
}

fn merge_json(path: &PathBuf, key: &str, value: Value) -> bool {
    let mut existing: Value = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !existing.is_object() {
        existing = serde_json::json!({});
    }
    let obj = existing.as_object_mut().unwrap();
    let servers = obj
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(m) = servers.as_object_mut() {
        m.insert(key.to_string(), value);
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, serde_json::to_string_pretty(&existing).unwrap_or_default() + "\n").is_ok()
}

fn merge_opencode_mcp(path: &PathBuf, key: &str, entry: Value) -> bool {
    let mut existing: Value = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !existing.is_object() {
        existing = serde_json::json!({});
    }
    let obj = existing.as_object_mut().unwrap();
    let mcp = obj
        .entry("mcp")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(m) = mcp.as_object_mut() {
        if !m.contains_key(key) {
            let command = entry.get("command").and_then(|c| c.as_str()).map(|s| s.to_string());
            let args: Vec<String> = entry
                .get("args")
                .and_then(|a| a.as_array())
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let mut cmd = vec![command.unwrap_or_else(|| "engram".to_string())];
            cmd.extend(args);
            m.insert(
                key.to_string(),
                serde_json::json!({
                    "command": cmd,
                    "enabled": true,
                    "type": "local",
                }),
            );
        }
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, serde_json::to_string_pretty(&existing).unwrap_or_default() + "\n").is_ok()
}

fn write_if_absent(path: &PathBuf, content: &str) -> bool {
    if path.exists() {
        return false;
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(path, content).is_ok()
}

/// Per-host rule targets. Claude Code writes to its global rules folder
/// (affects every project). Everyone else has no such global folder — they
/// get project-local files instead, and only when absent, since a project
/// file is more sensitive to clobber than a per-user dotfile. Continue has
/// no dedicated rules convention (per research), so it gets none.
pub(crate) fn rule_targets(host: &str) -> Vec<(PathBuf, String)> {
    let joined = || rule_text::all().join("\n\n");
    match host {
        "claude-code" => {
            let dir = home().join(".claude").join("rules");
            vec![
                (dir.join("exa.md"), rule_text::EXA.to_string()),
                (dir.join("git.md"), rule_text::GIT.to_string()),
                (dir.join("lean-ctx.md"), rule_text::LEANCTX.to_string()),
                (dir.join("engram.md"), rule_text::ENGRAM.to_string()),
            ]
        }
        "cursor" => {
            let content = format!("---\nalwaysApply: true\n---\n\n{}", joined());
            vec![(cwd().join(".cursor").join("rules").join("agentflare.mdc"), content)]
        }
        "codex" => {
            let content = format!("# Rules (agentflare)\n\n{}\n", joined());
            vec![(cwd().join("AGENTS.md"), content)]
        }
        "windsurf" => {
            vec![(cwd().join(".windsurf").join("rules").join("agentflare.md"), joined() + "\n")]
        }
        "vscode-copilot" => {
            vec![(cwd().join(".github").join("copilot-instructions.md"), joined() + "\n")]
        }
        "cline" => {
            vec![(cwd().join(".clinerules").join("agentflare.md"), joined() + "\n")]
        }
        "opencode" => {
            let dir = home().join(".config").join("opencode").join("rules");
            vec![
                (dir.join("exa.md"), rule_text::EXA.to_string()),
                (dir.join("git.md"), rule_text::GIT.to_string()),
                (dir.join("lean-ctx.md"), rule_text::LEANCTX.to_string()),
                (dir.join("engram.md"), rule_text::ENGRAM.to_string()),
            ]
        }
        _ => vec![], // "continue" — no dedicated rules convention found
    }
}

/// Per-host completion marker for components whose "done" state can't be
/// read back from the target's own config (or where re-checking would need
/// per-host format parsing). engram-setup specifically: `engram_installed()`
/// alone can't tell "set up for THIS host" from "set up for some other host
/// on this machine" once the binary exists globally.
fn host_marker(component: &str, host: &str) -> PathBuf {
    crate::state::state_dir().join(format!("{component}-{host}.done"))
}

fn mark_done(path: &PathBuf) {
    let _ = fs::create_dir_all(path.parent().unwrap());
    let _ = fs::write(path, format!("{:?}", std::time::SystemTime::now()));
}

/// Every skill name the shared skill_registry cache currently knows about —
/// same source `skill_search`/`skill_load` (mcp_server.rs) already serve
/// from, so "known skills" here always matches what those tools can find.
#[cfg(feature = "skill-overrides-sync")]
fn discover_skill_names() -> Result<Vec<String>, String> {
    let mut registry = skill_registry::Registry::open_default(&crate::paths::skills_db_path())
        .map_err(|e| e.to_string())?;
    registry.ensure_fresh().map_err(|e| e.to_string())?;
    registry.list_all_names().map_err(|e| e.to_string())
}

/// Pure merge step: adds a `"name-only"` entry for every name that doesn't
/// already have *some* skillOverrides entry — a skill the user (or another
/// tool) already set to e.g. `"off"` is left untouched. Returns how many
/// entries were newly added. Split out from `sync_skill_overrides` so this
/// logic is unit-testable without touching the real settings.json/skills.db.
#[cfg(feature = "skill-overrides-sync")]
fn apply_skill_overrides(names: &[String], settings: &mut Value) -> Result<usize, String> {
    if !settings.is_object() {
        *settings = serde_json::json!({});
    }
    let obj = settings.as_object_mut().expect("just ensured object above");
    let overrides = obj
        .entry("skillOverrides")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("skillOverrides is not an object")?;
    let mut added = 0;
    for name in names {
        if !overrides.contains_key(name) {
            overrides.insert(name.clone(), serde_json::json!("name-only"));
            added += 1;
        }
    }
    Ok(added)
}

/// Adds a `"name-only"` entry to `~/.claude/settings.json`'s `skillOverrides`
/// for every discovered skill that doesn't already have one.
#[cfg(feature = "skill-overrides-sync")]
fn sync_skill_overrides() -> Result<usize, String> {
    let names = discover_skill_names()?;
    let path = home().join(".claude").join("settings.json");
    let mut settings = claude_settings();
    let added = apply_skill_overrides(&names, &mut settings)?;
    if added > 0 {
        fs::write(&path, serde_json::to_string_pretty(&settings).unwrap_or_default() + "\n")
            .map_err(|e| e.to_string())?;
    }
    Ok(added)
}

pub fn get_components(host: &str) -> Vec<Component> {
    let claude_code_only = host == "claude-code";
    let host_owned = host.to_string();
    let host_owned2 = host.to_string();
    let leanctx_log = crate::state::state_dir().join("leanctx-install.log");
    let ponytail_config = home().join(".config").join("ponytail").join("config.json");
    let caveman_config = home().join(".config").join("caveman").join("config.json");

    #[cfg_attr(not(feature = "skill-overrides-sync"), allow(unused_mut))]
    let mut components = vec![
        Component {
            id: "rules",
            needs_consent: false,
            describe: format!("usage rules for {host}"),
            check: {
                let host = host_owned.clone();
                Box::new(move || {
                    let targets = rule_targets(&host);
                    !targets.is_empty() && targets.iter().all(|(p, _)| p.exists())
                })
            },
            apply: {
                let host = host_owned.clone();
                Box::new(move || {
                    let mut written = vec![];
                    for (path, content) in rule_targets(&host) {
                        if !path.exists()
                            && write_if_absent(&path, &format!("{content}\n"))
                        {
                            written.push(path.file_name().unwrap().to_string_lossy().to_string());
                        }
                    }
                    if written.is_empty() {
                        "rules already present (or none defined for this host)".to_string()
                    } else {
                        format!("rules written: {}", written.join(", "))
                    }
                })
            },
        },
        Component {
            id: "leanctx",
            needs_consent: true,
            // lean-ctx's own `onboard` command wires MCP into whichever
            // supported tool it detects, so no per-host branching needed
            // here — same as engram, trust the upstream tool's own setup.
            describe: "lean-ctx (context compression) — npm install -g lean-ctx-bin && lean-ctx onboard".to_string(),
            check: Box::new(|| run_ok(if cfg!(windows) { "where" } else { "which" }, &["lean-ctx"])),
            apply: {
                let log = leanctx_log.clone();
                Box::new(move || {
                    if log.exists() {
                        return format!("lean-ctx install already triggered — check {}", log.display());
                    }
                    let _ = fs::create_dir_all(log.parent().unwrap());
                    let cmd = "npm install -g lean-ctx-bin && lean-ctx onboard";
                    let result = if cfg!(windows) {
                        Command::new("cmd").args(["/c", cmd]).status()
                    } else {
                        Command::new("sh").args(["-c", cmd]).status()
                    };
                    let _ = fs::write(&log, format!("{:?}", std::time::SystemTime::now()));
                    match result {
                        Ok(s) if s.success() => "lean-ctx installed and onboarded".to_string(),
                        _ => "lean-ctx install failed — run manually: npm install -g lean-ctx-bin && lean-ctx onboard".to_string(),
                    }
                })
            },
        },
        Component {
            id: "engram",
            needs_consent: true,
            describe: if claude_code_only {
                "engram (cross-session memory) — claude plugin marketplace add Gentleman-Programming/engram && claude plugin install engram".to_string()
            } else if ENGRAM_NATIVE_HOSTS.contains(&host) {
                format!("engram (cross-session memory) — engram setup {host} (auto-installs engram itself first via go install/brew if missing)")
            } else {
                format!("engram (cross-session memory) — manual MCP registration (no native `engram setup {host}`), auto-installs engram itself via go install/brew if missing")
            },
            check: {
                let host = host_owned2.clone();
                Box::new(move || {
                    if host == "claude-code" {
                        return plugin_enabled(&claude_settings(), "engram@engram");
                    }
                    // Binary existing globally isn't enough — this specific
                    // host's setup/registration must have run too.
                    engram_install::engram_installed() && host_marker("engram-setup", &host).exists()
                })
            },
            apply: {
                let host = host_owned2.clone();
                Box::new(move || {
                    if host == "claude-code" {
                        let ok = run_ok("claude", &["plugin", "marketplace", "add", "Gentleman-Programming/engram"])
                            && run_ok("claude", &["plugin", "install", "engram"]);
                        return if ok {
                            "engram plugin installed — restart to activate".to_string()
                        } else {
                            "engram plugin install failed — run manually: claude plugin marketplace add Gentleman-Programming/engram && claude plugin install engram".to_string()
                        };
                    }

                    if !engram_install::engram_installed() {
                        return match engram_install::install_and_setup(&host) {
                            engram_install::InstallOutcome::Started(m) => m,
                            engram_install::InstallOutcome::NoSafePath(m) => m,
                        };
                    }

                    let marker = host_marker("engram-setup", &host);

                    if ENGRAM_NATIVE_HOSTS.contains(&host.as_str()) {
                        return if run_ok("engram", &["setup", &host]) {
                            mark_done(&marker);
                            format!("engram setup {host} done")
                        } else {
                            format!("engram setup {host} failed — run manually: engram setup {host}")
                        };
                    }

                    // cline/continue/opencode: no native `engram setup` —
                    // register the MCP command directly in the host's
                    // config, same shape engram's docs use for "any
                    // other MCP client".
                    let entry = serde_json::json!({ "command": "engram", "args": ENGRAM_MCP_ARGS });
                    let result = match host.as_str() {
                        "cline" => {
                            let path = home().join(".cline").join("mcp.json");
                            if merge_json(&path, "engram", entry) {
                                format!("{} (engram registered)", path.display())
                            } else {
                                format!("failed to write {}", path.display())
                            }
                        }
                        "continue" => {
                            let path = cwd().join(".continue").join("mcpServers").join("engram.json");
                            if write_if_absent(&path, &(serde_json::to_string_pretty(&entry).unwrap() + "\n")) {
                                format!("{} written", path.display())
                            } else {
                                format!("{} exists, skipped", path.display())
                            }
                        }
                        "opencode" => {
                            let path = home().join(".config").join("opencode").join("opencode.jsonc");
                            if merge_opencode_mcp(&path, "engram", entry) {
                                format!("{} (engram registered)", path.display())
                            } else {
                                format!("failed to write {}", path.display())
                            }
                        }
                        _ => format!("no engram integration defined for host '{host}'"),
                    };
                    mark_done(&marker);
                    result
                })
            },
        },
        // agentflare's own MCP server exposes skill_search/skill_load — the
        // on-demand replacement for the always-listed skill descriptions
        // this same init wires `skillOverrides` to suppress (below). Same 4
        // hosts engram's manual-registration branch covers; other hosts
        // report satisfied until their MCP config format is verified here.
        Component {
            id: "agentflare-mcp",
            needs_consent: true,
            describe: if host_owned == "claude-code" {
                "agentflare MCP server (skill_search/skill_load) — claude mcp add agentflare -- agentflare mcp".to_string()
            } else if matches!(host_owned.as_str(), "cline" | "continue" | "opencode") {
                format!("agentflare MCP server (skill_search/skill_load) — manual MCP registration for {host_owned}")
            } else {
                "agentflare MCP server — not yet supported for this host".to_string()
            },
            check: {
                let host = host_owned.clone();
                Box::new(move || match host.as_str() {
                    "claude-code" => claude_json()
                        .get("mcpServers")
                        .and_then(|m| m.get("agentflare"))
                        .is_some(),
                    "cline" => json_at(&home().join(".cline").join("mcp.json"))
                        .get("mcpServers")
                        .and_then(|m| m.get("agentflare"))
                        .is_some(),
                    "continue" => cwd().join(".continue").join("mcpServers").join("agentflare.json").exists(),
                    "opencode" => json_at(&home().join(".config").join("opencode").join("opencode.jsonc"))
                        .get("mcp")
                        .and_then(|m| m.get("agentflare"))
                        .is_some(),
                    _ => true,
                })
            },
            apply: {
                let host = host_owned.clone();
                Box::new(move || {
                    let entry = serde_json::json!({ "command": "agentflare", "args": ["mcp"] });
                    match host.as_str() {
                        "claude-code" => {
                            if run_ok("claude", &["mcp", "add", "agentflare", "-s", "user", "--", "agentflare", "mcp"]) {
                                "agentflare MCP server registered with claude-code".to_string()
                            } else {
                                "agentflare MCP registration failed — run manually: claude mcp add agentflare -- agentflare mcp".to_string()
                            }
                        }
                        "cline" => {
                            let path = home().join(".cline").join("mcp.json");
                            if merge_json(&path, "agentflare", entry) {
                                format!("{} (agentflare registered)", path.display())
                            } else {
                                format!("failed to write {}", path.display())
                            }
                        }
                        "continue" => {
                            let path = cwd().join(".continue").join("mcpServers").join("agentflare.json");
                            if write_if_absent(&path, &(serde_json::to_string_pretty(&entry).unwrap() + "\n")) {
                                format!("{} written", path.display())
                            } else {
                                format!("{} exists, skipped", path.display())
                            }
                        }
                        "opencode" => {
                            let path = home().join(".config").join("opencode").join("opencode.jsonc");
                            if merge_opencode_mcp(&path, "agentflare", entry) {
                                format!("{} (agentflare registered)", path.display())
                            } else {
                                format!("failed to write {}", path.display())
                            }
                        }
                        _ => format!("no agentflare MCP integration defined for host '{host}'"),
                    }
                })
            },
        },
        // Ponytail/Caveman are Claude Code plugins installed via the `claude
        // plugin` CLI — no equivalent exists on any other host, so these
        // report "satisfied" everywhere else rather than nagging about
        // something that can't be installed there.
        Component {
            id: "ponytail-plugin",
            needs_consent: true,
            describe: "Ponytail plugin — claude plugin marketplace add DietrichGebert/ponytail && claude plugin install ponytail@ponytail".to_string(),
            check: Box::new(move || !claude_code_only || plugin_enabled(&claude_settings(), "ponytail@ponytail")),
            apply: Box::new(|| {
                let ok = run_ok("claude", &["plugin", "marketplace", "add", "DietrichGebert/ponytail"])
                    && run_ok("claude", &["plugin", "install", "ponytail@ponytail"]);
                if ok {
                    "Ponytail plugin installed — restart to activate".to_string()
                } else {
                    "Ponytail plugin install failed — run manually".to_string()
                }
            }),
        },
        Component {
            id: "ponytail-mode",
            needs_consent: false,
            describe: "pin Ponytail to ultra mode".to_string(),
            check: {
                let path = ponytail_config.clone();
                Box::new(move || {
                    if !claude_code_only {
                        return true;
                    }
                    fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .and_then(|v| v.get("defaultMode").cloned())
                        .is_some()
                })
            },
            apply: {
                let path = ponytail_config.clone();
                Box::new(move || {
                    if write_pinned_mode(&path) {
                        "Ponytail pinned to ultra".to_string()
                    } else {
                        "Ponytail mode already set".to_string()
                    }
                })
            },
        },
        Component {
            id: "caveman-mode",
            needs_consent: false,
            describe: "pin Caveman to ultra mode".to_string(),
            check: {
                let path = caveman_config.clone();
                Box::new(move || {
                    if !claude_code_only {
                        return true;
                    }
                    if !plugin_enabled(&claude_settings(), "caveman@caveman") {
                        return true; // nothing to pin yet
                    }
                    fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .and_then(|v| v.get("defaultMode").and_then(|m| m.as_str()).map(String::from))
                        == Some("ultra".to_string())
                })
            },
            apply: {
                let path = caveman_config.clone();
                Box::new(move || {
                    if write_pinned_mode(&path) {
                        "Caveman pinned to ultra".to_string()
                    } else {
                        "Caveman mode already set".to_string()
                    }
                })
            },
        },
    ];

    // Gated behind the `skill-overrides-sync` cargo feature (off by
    // default, not part of released builds) until we have real evidence
    // this saves money rather than just cache-cheap context tokens (measured
    // ~900 tokens/turn of context-window space, mostly cache reads).
    // Suppresses every known skill's description from
    // Claude Code's always-on listing (settings.json `skillOverrides:
    // name-only`) — names stay typable, skill_search/skill_load
    // (registered above) become the on-demand detail source.
    // Claude-Code-only: other hosts have no equivalent per-skill override
    // mechanism. Not consent-gated (a local config tweak, same trust
    // level as ponytail-mode/caveman-mode above) so it also re-syncs on
    // every session-start as new skills appear, not just during `init`.
    #[cfg(feature = "skill-overrides-sync")]
    {
        let host = host_owned.clone();
        components.push(Component {
            id: "skill-overrides-sync",
            needs_consent: false,
            describe: "sync skillOverrides so newly-discovered skills defer their description to on-demand search".to_string(),
            check: Box::new(move || {
                if host != "claude-code" {
                    return true;
                }
                let Ok(names) = discover_skill_names() else { return true };
                let settings = claude_settings();
                let overrides = settings.get("skillOverrides").and_then(|v| v.as_object());
                names.iter().all(|n| overrides.is_some_and(|o| o.contains_key(n)))
            }),
            apply: Box::new(|| match sync_skill_overrides() {
                Ok(0) => "skillOverrides already up to date".to_string(),
                Ok(n) => format!("skillOverrides: {n} skill(s) set to name-only"),
                Err(e) => format!("skillOverrides sync failed: {e}"),
            }),
        });
    }

    components
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTS: &[&str] = &[
        "claude-code",
        "codex",
        "cursor",
        "windsurf",
        "vscode-copilot",
        "cline",
        "continue",
        "opencode",
    ];

    #[test]
    fn every_host_gets_the_full_component_set() {
        // "skill-overrides-sync" only exists behind the `skill-overrides-sync`
        // cargo feature (opt-in, not part of released builds — unconfirmed
        // $ savings, see Cargo.toml). Expected ids adjust accordingly.
        #[cfg(not(feature = "skill-overrides-sync"))]
        let expected: Vec<&str> = vec![
            "rules",
            "leanctx",
            "engram",
            "agentflare-mcp",
            "ponytail-plugin",
            "ponytail-mode",
            "caveman-mode",
        ];
        #[cfg(feature = "skill-overrides-sync")]
        let expected: Vec<&str> = vec![
            "rules",
            "leanctx",
            "engram",
            "agentflare-mcp",
            "ponytail-plugin",
            "ponytail-mode",
            "caveman-mode",
            "skill-overrides-sync",
        ];

        for host in HOSTS {
            let components = get_components(host);
            assert_eq!(
                components.len(),
                expected.len(),
                "expected {} components for host '{host}', got {}",
                expected.len(),
                components.len()
            );
            let ids: Vec<_> = components.iter().map(|c| c.id).collect();
            assert_eq!(ids, expected);
        }
    }

    #[test]
    fn rule_targets_are_project_local_except_claude_code_and_opencode() {
        // claude-code writes to the global rules dir under ~/.claude/rules.
        let cc_targets = rule_targets("claude-code");
        assert!(!cc_targets.is_empty());
        for (path, _) in &cc_targets {
            assert!(path.to_string_lossy().contains(".claude"));
        }

        // opencode writes to the global rules dir under ~/.config/opencode/rules.
        let oc_targets = rule_targets("opencode");
        assert!(!oc_targets.is_empty());
        for (path, _) in &oc_targets {
            assert!(path.to_string_lossy().contains("opencode"));
        }

        // Every other defined host writes a project-local path — check for
        // the actual per-host marker dir, not "starts with home" (the repo
        // itself can live under home, which would make that check useless).
        let expectations = [
            ("cursor", ".cursor"),
            ("codex", "AGENTS.md"),
            ("windsurf", ".windsurf"),
            ("vscode-copilot", ".github"),
            ("cline", ".clinerules"),
        ];
        for (host, marker) in expectations {
            let targets = rule_targets(host);
            assert!(!targets.is_empty(), "expected rule targets for '{host}'");
            for (path, _) in &targets {
                assert!(
                    path.to_string_lossy().contains(marker),
                    "'{host}' rule target {path:?} should contain '{marker}'"
                );
            }
        }

        // "continue" has no dedicated rules convention — empty on purpose.
        assert!(rule_targets("continue").is_empty());
    }

    #[test]
    fn non_claude_code_hosts_never_need_the_claude_cli_for_ponytail_or_caveman() {
        // Regression check for the host-gating bug caught during manual
        // testing: these two components must report "satisfied" (no
        // pending nag, no attempted install) on every host except
        // claude-code, since Ponytail/Caveman have no equivalent elsewhere.
        for host in ["codex", "cursor", "windsurf", "vscode-copilot", "cline", "continue", "opencode"] {
            let components = get_components(host);
            let ponytail_plugin = components.iter().find(|c| c.id == "ponytail-plugin").unwrap();
            let caveman_mode = components.iter().find(|c| c.id == "caveman-mode").unwrap();
            assert!((ponytail_plugin.check)(), "ponytail-plugin should be satisfied on '{host}'");
            assert!((caveman_mode.check)(), "caveman-mode should be satisfied on '{host}'");
        }
    }

    #[test]
    fn agentflare_mcp_reports_satisfied_on_hosts_without_a_wired_format() {
        // codex/cursor/windsurf/vscode-copilot have no verified MCP config
        // format wired here yet — must never nag or attempt registration.
        for host in ["codex", "cursor", "windsurf", "vscode-copilot"] {
            let components = get_components(host);
            let agentflare_mcp = components.iter().find(|c| c.id == "agentflare-mcp").unwrap();
            assert!((agentflare_mcp.check)(), "agentflare-mcp should be satisfied on '{host}'");
        }
    }

    #[test]
    #[cfg(feature = "skill-overrides-sync")]
    fn skill_overrides_sync_reports_satisfied_on_non_claude_code_hosts() {
        for host in ["codex", "cursor", "windsurf", "vscode-copilot", "cline", "continue", "opencode"] {
            let components = get_components(host);
            let sync = components.iter().find(|c| c.id == "skill-overrides-sync").unwrap();
            assert!((sync.check)(), "skill-overrides-sync should be satisfied on '{host}'");
        }
    }

    #[test]
    #[cfg(feature = "skill-overrides-sync")]
    fn apply_skill_overrides_adds_name_only_for_new_skills_only() {
        let mut settings = serde_json::json!({
            "skillOverrides": { "already-configured": "off" }
        });
        let names = vec!["already-configured".to_string(), "brand-new".to_string()];
        let added = apply_skill_overrides(&names, &mut settings).unwrap();
        assert_eq!(added, 1);
        assert_eq!(settings["skillOverrides"]["already-configured"], "off");
        assert_eq!(settings["skillOverrides"]["brand-new"], "name-only");
    }

    #[test]
    #[cfg(feature = "skill-overrides-sync")]
    fn apply_skill_overrides_handles_missing_settings_object() {
        let mut settings = Value::Null;
        let added = apply_skill_overrides(&["some-skill".to_string()], &mut settings).unwrap();
        assert_eq!(added, 1);
        assert_eq!(settings["skillOverrides"]["some-skill"], "name-only");
    }

    #[test]
    #[cfg(feature = "skill-overrides-sync")]
    fn apply_skill_overrides_is_idempotent() {
        let mut settings = serde_json::json!({});
        let names = vec!["skill-a".to_string()];
        assert_eq!(apply_skill_overrides(&names, &mut settings).unwrap(), 1);
        assert_eq!(apply_skill_overrides(&names, &mut settings).unwrap(), 0);
    }
}
