// Detects the agentflare Claude Code plugin, whose mod replaces the command
// hooks for `MOD_COVERED_EVENTS` (see `wire_claude_code_with`).
use serde_json::Value;
use std::path::Path;

/// Hook events the plugin's mod handles itself; `init` leaves them unwired
/// when the plugin is installed and `doctor` flags them if still present.
/// `Stop` (inter-agent mail, busy/idle liveness) and `PostToolUseFailure`
/// (friction nudges, failure recall) are not covered by the mod and stay wired.
pub(crate) const MOD_COVERED_EVENTS: &[&str] =
    &["PreToolUse", "PostToolUse", "PreCompact", "SubagentStart"];

const PLUGIN_NAME: &str = "agentflare";

/// True when the agentflare Claude plugin is installed AND enabled, so its
/// mod (the guard) actually runs: a cached copy under
/// `~/.claude/plugins/cache/<market>/agentflare` whose
/// `enabledPlugins["agentflare@<market>"]` is `true` in
/// `~/.claude/settings.json`, or an enabled entry in `claude plugin list
/// --json` (2 s budget). A disabled or uninstalled-but-cached plugin does not
/// count: `init` must then wire the hooks, or nothing guards the session.
pub(crate) fn claude_plugin_installed() -> bool {
    let claude = crate::paths::claude_dir();
    let settings = std::fs::read_to_string(claude.join("settings.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    cached_markets(&claude.join("plugins").join("cache"))
        .iter()
        .any(|market| enabled_in_settings(&settings, market))
        || listed_by_claude_cli()
}

/// Marketplaces whose cache holds the plugin's manifest, flat
/// (`<cache>/<market>/agentflare/.claude-plugin/plugin.json`) or versioned
/// (`<cache>/<market>/agentflare/<version>/.claude-plugin/plugin.json`).
fn cached_markets(cache: &Path) -> Vec<String> {
    let has_manifest = |d: &Path| d.join(".claude-plugin").join("plugin.json").is_file();
    let Ok(markets) = std::fs::read_dir(cache) else {
        return Vec::new();
    };
    markets
        .flatten()
        .filter(|m| {
            let dir = m.path().join(PLUGIN_NAME);
            has_manifest(&dir)
                || std::fs::read_dir(&dir)
                    .is_ok_and(|vs| vs.flatten().any(|v| has_manifest(&v.path())))
        })
        .filter_map(|m| m.file_name().to_str().map(str::to_string))
        .collect()
}

/// `enabledPlugins["agentflare@<market>"] == true`; absent, false or an
/// unreadable settings file all mean not enabled.
fn enabled_in_settings(settings: &Value, market: &str) -> bool {
    settings["enabledPlugins"][format!("{PLUGIN_NAME}@{market}")].as_bool() == Some(true)
}

/// Whether `claude plugin list --json` output names the plugin, as either a
/// top-level array or `{ "plugins": [...] }`, by `name` or `<name>@<market>`
/// id, and does not mark it disabled (`"enabled": false`; absent = enabled).
fn list_names_plugin(json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return false;
    };
    let items = v.get("plugins").unwrap_or(&v).as_array();
    items.into_iter().flatten().any(|p| {
        let named = ["name", "id"].iter().any(|k| {
            p.get(k).and_then(Value::as_str).is_some_and(|s| {
                s == PLUGIN_NAME
                    || s.strip_prefix(PLUGIN_NAME)
                        .is_some_and(|r| r.starts_with('@'))
            })
        });
        named && p.get("enabled").and_then(Value::as_bool) != Some(false)
    })
}

// Tests only see the cache: spawning the developer's real `claude` would make
// them depend on what happens to be installed on the machine.
#[cfg(test)]
fn listed_by_claude_cli() -> bool {
    false
}

#[cfg(not(test))]
fn listed_by_claude_cli() -> bool {
    // Probed once per process: `init` asks twice (lifecycle + optimize wiring).
    static LISTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LISTED.get_or_init(|| {
        let cwd = std::env::current_dir().unwrap_or_default();
        let run = |program: &str| {
            flare_git_core::worktree::run_output_timeout_env(
                program,
                &["plugin", "list", "--json"],
                &cwd,
                2,
                &[],
            )
        };
        let out = run("claude");
        // An npm install leaves only a `claude.cmd` shim on PATH, which a bare
        // `claude` spawn does not resolve on Windows. Still fail-open.
        #[cfg(windows)]
        let out = match out {
            Err(e) if e.contains("spawn failed") => run("claude.cmd"),
            other => other,
        };
        out.is_ok_and(|out| {
            out.status.success() && list_names_plugin(&String::from_utf8_lossy(&out.stdout))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::paths::test_support::with_temp_home;

    fn cache_manifest(dir: &Path) {
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(dir.join(".claude-plugin").join("plugin.json"), "{}").unwrap();
    }

    #[test]
    fn cache_search_finds_plugin_dir_with_or_without_version() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mkt").join("agentflare").join("1.0.0");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(cached_markets(tmp.path()).is_empty());
        cache_manifest(&dir);
        assert_eq!(cached_markets(tmp.path()), ["mkt"]);
        let flat = tempfile::tempdir().unwrap();
        cache_manifest(&flat.path().join("mkt").join("other"));
        assert!(cached_markets(flat.path()).is_empty());
        cache_manifest(&flat.path().join("mkt2").join("agentflare"));
        assert_eq!(cached_markets(flat.path()), ["mkt2"]);
    }

    #[test]
    fn list_output_matches_name_or_marketplace_id() {
        assert!(list_names_plugin(r#"[{"name":"agentflare"}]"#));
        assert!(list_names_plugin(r#"[{"id":"agentflare@market"}]"#));
        assert!(list_names_plugin(r#"{"plugins":[{"name":"agentflare"}]}"#));
        assert!(list_names_plugin(
            r#"[{"id":"agentflare@market","enabled":true}]"#
        ));
        assert!(!list_names_plugin(
            r#"[{"id":"agentflare@market","enabled":false}]"#
        ));
        assert!(!list_names_plugin(r#"[{"name":"agentflare-other"}]"#));
        assert!(!list_names_plugin("not json"));
    }

    /// Cached at `~/.claude/plugins/cache/mkt/agentflare/1.0.0`, with
    /// `settings` (None = no file) as `~/.claude/settings.json`.
    fn installed_with(settings: Option<&str>) -> bool {
        let claude = crate::paths::claude_dir();
        let cache = claude.join("plugins").join("cache");
        cache_manifest(&cache.join("mkt").join("agentflare").join("1.0.0"));
        if let Some(s) = settings {
            std::fs::write(claude.join("settings.json"), s).unwrap();
        }
        claude_plugin_installed()
    }

    #[test]
    fn claude_plugin_cached_and_enabled_is_installed() {
        with_temp_home(|| {
            assert!(installed_with(Some(
                r#"{"enabledPlugins":{"agentflare@mkt":true}}"#
            )));
        });
    }

    #[test]
    fn claude_plugin_cached_but_disabled_is_not_installed() {
        with_temp_home(|| {
            assert!(!installed_with(Some(
                r#"{"enabledPlugins":{"agentflare@mkt":false}}"#
            )));
        });
    }

    #[test]
    fn claude_plugin_cached_but_never_enabled_is_not_installed() {
        with_temp_home(|| assert!(!installed_with(None)));
        with_temp_home(|| {
            assert!(!installed_with(Some(
                r#"{"enabledPlugins":{"agentflare@other":true}}"#
            )));
        });
        with_temp_home(|| assert!(!installed_with(Some("{ not json"))));
    }
}
