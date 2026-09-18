// Home-directory resolution now lives in `agentflare-config` (item #257's
// config-management migration); this stays as a thin wrapper so the many
// existing `paths::home()` call sites don't need to change.
use std::path::PathBuf;

pub fn home() -> PathBuf {
    agentflare_config::home()
}

/// `~/.agentflare` — agentflare's own per-user data dir (holding
/// `agentflare.db`, `store.db`, `backend.db`, artifacts, staging, shims,
/// config.toml, ...). Single definition, mirroring `claude_dir()` below, so
/// the 20+ existing call sites can't drift apart if this root ever moves.
pub fn agentflare_dir() -> PathBuf {
    home().join(".agentflare")
}

/// Absolute path to the currently-running agentflare binary, falling back to
/// the bare name if it can't be resolved. Used wherever agentflare registers
/// itself as a command in another tool's config (Claude Code hooks, MCP
/// servers) so the integration keeps working even when the launching process
/// doesn't inherit agentflare's install dir on PATH — e.g. a GUI-launched
/// Claude Code that never sourced the shell profile that adds ~/.local/bin.
pub fn agentflare_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "agentflare".to_string())
}

/// `~/.claude` — the Claude Code config directory. Single definition so the
/// many call sites that build paths under it (settings, rules, credentials)
/// can't drift apart.
pub fn claude_dir() -> PathBuf {
    home().join(".claude")
}

/// `~/.claude/settings.json` — Claude Code's user-scope settings/hooks file.
pub fn claude_settings_path() -> PathBuf {
    claude_dir().join("settings.json")
}

/// `~/.claude/rules` — where agentflare drops its rule markdown for Claude Code.
pub fn claude_rules_dir() -> PathBuf {
    claude_dir().join("rules")
}

/// `~/.claude/projects` — Claude Code's per-project session transcript
/// directories, each containing `<session_id>.jsonl` files.
pub fn claude_projects_dir() -> PathBuf {
    claude_dir().join("projects")
}

/// `~/.claude.json` — user-scope `claude mcp add` registrations live here, a
/// separate file from `~/.claude/settings.json`.
pub fn claude_json_path() -> PathBuf {
    home().join(".claude.json")
}

/// `~/.config/opencode` — the OpenCode config directory.
pub fn opencode_dir() -> PathBuf {
    home().join(".config").join("opencode")
}

/// `~/.config/opencode/opencode.jsonc` — the file agentflare itself owns and
/// writes to. A hand-maintained sibling `opencode.json` (see
/// `opencode_json_path`) commonly carries the user's own MCP/plugin entries;
/// opencode deep-merges both, but agentflare only ever writes this one.
pub fn opencode_config_path() -> PathBuf {
    opencode_dir().join("opencode.jsonc")
}

/// `~/.config/opencode/opencode.json` — opencode's other config file.
/// Read-only from agentflare's side: idempotency checks must look here too
/// (via `components::opencode_config_merged`) so a value the user hand-added
/// here isn't seen as "missing" and re-written into `opencode.jsonc`, but
/// writes always target `opencode_config_path` instead of this file.
pub fn opencode_json_path() -> PathBuf {
    opencode_dir().join("opencode.json")
}

/// `~/.config/opencode/rules` — where agentflare drops its rule markdown for
/// OpenCode.
pub fn opencode_rules_dir() -> PathBuf {
    opencode_dir().join("rules")
}

/// `~/.config/opencode/plugin` — opencode auto-loads every plugin file
/// dropped directly in this directory (singular "plugin"; the similarly
/// named "plugins" is not an opencode convention and isn't scanned).
pub fn opencode_plugin_dir() -> PathBuf {
    opencode_dir().join("plugin")
}

/// OpenCode's data directory (where `auth.json` lives), distinct from the
/// config directory (`opencode_dir`) — mirrors opencode's own resolution:
/// `OPENCODE_DATA_DIR`, else `XDG_DATA_HOME/opencode`, else the platform
/// default (`~/.local/share/opencode` on Linux/Windows,
/// `~/Library/Application Support/opencode` on macOS).
pub fn opencode_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OPENCODE_DATA_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("XDG_DATA_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("opencode");
    }
    #[cfg(target_os = "macos")]
    {
        home()
            .join("Library")
            .join("Application Support")
            .join("opencode")
    }
    #[cfg(not(target_os = "macos"))]
    {
        home().join(".local").join("share").join("opencode")
    }
}

/// `auth.json` inside OpenCode's data directory — opencode's provider
/// credentials, including the `opencode-go` subscription API key the Go
/// usage tracker reads.
pub fn opencode_auth_path() -> PathBuf {
    opencode_data_dir().join("auth.json")
}

/// Shared by mcp_server.rs (serving skill_search/skill_load) and
/// components.rs (syncing skillOverrides) — same on-disk cache, single path
/// definition so the two can never drift apart.
pub fn skills_db_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("agentflare")
        .join("skills.db")
}

// Shared by state.rs/init.rs tests: both AGENTFLARE_HOME_OVERRIDE and cwd are
// process-global, so tests that touch either must run serialized against
// each other or they'll stomp on one another under cargo's default
// parallel test runner.
#[cfg(test)]
pub(crate) mod test_support {
    // The actual with_temp_home implementation now lives in
    // `agentflare-config`; re-exported here so the many existing
    // `crate::paths::test_support::with_temp_home` call sites don't need to
    // change.
    pub(crate) use agentflare_config::test_support::with_temp_home;

    // Same GLOBAL_STATE_LOCK reasoning as agentflare_config::test_support's
    // with_temp_home -- cwd is also process-global.
    use agent_registry::detect::PATH_LOCK as GLOBAL_STATE_LOCK;
    use std::path::PathBuf;

    // Restores the original cwd on drop -- including on unwind, same
    // reasoning as ResetHomeOverrideOnDrop above.
    struct RestoreCwdOnDrop(PathBuf);
    impl Drop for RestoreCwdOnDrop {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }

    pub(crate) fn with_temp_cwd<T>(f: impl FnOnce() -> T) -> T {
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Same reasoning as with_temp_home above: a unique dir per call
        // instead of a fixed shared name.
        let temp_dir = tempfile::tempdir().unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(temp_dir.path()).unwrap();
        let _restore = RestoreCwdOnDrop(original);
        f()
    }
}

// with_temp_home's own isolation guarantees (leftover-file safety, thread
// contention, env-var cleanup) are covered by agentflare-config's own tests
// now that the implementation lives there; this just checks the wrapper
// delegates correctly.
#[cfg(test)]
mod tests {
    use super::test_support::with_temp_home;

    #[test]
    fn home_respects_with_temp_home_override() {
        with_temp_home(|| {
            assert_eq!(super::home(), agentflare_config::home());
        });
    }
}
