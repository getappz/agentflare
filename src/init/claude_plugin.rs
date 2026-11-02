// Detects the agentflare Claude Code plugin, whose mod replaces the command
// hooks for `MOD_COVERED_EVENTS` (see `wire_claude_code_with`).
use serde_json::Value;
use std::path::Path;

/// Hook events the plugin's mod handles itself; `init` leaves them unwired
/// when the plugin is installed and `doctor` flags them if still present.
pub(crate) const MOD_COVERED_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PreCompact",
    "Stop",
    "SubagentStart",
];

const PLUGIN_NAME: &str = "agentflare";

/// True when the agentflare Claude plugin is installed: its manifest sits in
/// `~/.claude/plugins/cache`, or `claude plugin list --json` (2 s budget)
/// lists it.
pub(crate) fn claude_plugin_installed() -> bool {
    cache_has_plugin(&crate::paths::claude_dir().join("plugins").join("cache"), 0)
        || listed_by_claude_cli()
}

/// `<cache>/**/agentflare/[<version>/].claude-plugin/plugin.json`, searched at
/// most four directories deep (marketplace/plugin/version).
fn cache_has_plugin(dir: &Path, depth: u8) -> bool {
    let named = |p: &Path| p.file_name().is_some_and(|n| n == PLUGIN_NAME);
    if (named(dir) || dir.parent().is_some_and(named))
        && dir.join(".claude-plugin").join("plugin.json").is_file()
    {
        return true;
    }
    if depth >= 4 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|e| e.path().is_dir() && cache_has_plugin(&e.path(), depth + 1))
}

/// Whether `claude plugin list --json` output names the plugin, as either a
/// top-level array or `{ "plugins": [...] }`, by `name` or `<name>@<market>` id.
fn list_names_plugin(json: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return false;
    };
    let items = v.get("plugins").unwrap_or(&v).as_array();
    items.into_iter().flatten().any(|p| {
        ["name", "id"].iter().any(|k| {
            p.get(k).and_then(Value::as_str).is_some_and(|s| {
                s == PLUGIN_NAME
                    || s.strip_prefix(PLUGIN_NAME)
                        .is_some_and(|r| r.starts_with('@'))
            })
        })
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

    #[test]
    fn cache_search_finds_plugin_dir_with_or_without_version() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mkt").join("agentflare").join("1.0.0");
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        assert!(!cache_has_plugin(tmp.path(), 0));
        std::fs::write(dir.join(".claude-plugin").join("plugin.json"), "{}").unwrap();
        assert!(cache_has_plugin(tmp.path(), 0));
        let flat = tempfile::tempdir().unwrap();
        let other = flat.path().join("mkt").join("other");
        std::fs::create_dir_all(other.join(".claude-plugin")).unwrap();
        std::fs::write(other.join(".claude-plugin").join("plugin.json"), "{}").unwrap();
        assert!(!cache_has_plugin(flat.path(), 0));
        let direct = flat.path().join("mkt").join("agentflare");
        std::fs::create_dir_all(direct.join(".claude-plugin")).unwrap();
        std::fs::write(direct.join(".claude-plugin").join("plugin.json"), "{}").unwrap();
        assert!(cache_has_plugin(flat.path(), 0));
    }

    #[test]
    fn list_output_matches_name_or_marketplace_id() {
        assert!(list_names_plugin(r#"[{"name":"agentflare"}]"#));
        assert!(list_names_plugin(r#"[{"id":"agentflare@market"}]"#));
        assert!(list_names_plugin(r#"{"plugins":[{"name":"agentflare"}]}"#));
        assert!(!list_names_plugin(r#"[{"name":"agentflare-other"}]"#));
        assert!(!list_names_plugin("not json"));
    }
}
