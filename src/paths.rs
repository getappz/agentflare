// Home-directory resolution with an explicit test override. `dirs::home_dir()`
// resolves via the OS directly on Windows (SHGetKnownFolderPath) and ignores
// HOME/USERPROFILE env var overrides — learned the hard way when a
// "sandboxed" test run wrote real changes to a live ~/.claude/settings.json.
// AGENTFLARE_HOME_OVERRIDE is agentflare's own escape hatch for tests/CI.
use std::path::PathBuf;

pub fn home() -> PathBuf {
    if let Ok(p) = std::env::var("AGENTFLARE_HOME_OVERRIDE") {
        return PathBuf::from(p);
    }
    dirs::home_dir().expect("home directory not found")
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

/// `~/.claude.json` — user-scope `claude mcp add` registrations live here, a
/// separate file from `~/.claude/settings.json`.
pub fn claude_json_path() -> PathBuf {
    home().join(".claude.json")
}

/// `~/.config/opencode` — the OpenCode config directory.
pub fn opencode_dir() -> PathBuf {
    home().join(".config").join("opencode")
}

/// `~/.config/opencode/opencode.jsonc` — OpenCode's config file (JSONC).
pub fn opencode_config_path() -> PathBuf {
    opencode_dir().join("opencode.jsonc")
}

/// `~/.config/opencode/rules` — where agentflare drops its rule markdown for
/// OpenCode.
pub fn opencode_rules_dir() -> PathBuf {
    opencode_dir().join("rules")
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
    // One process-wide lock for ALL env mutation in this test binary.
    // src/agents.rs already serializes PATH edits on agent_registry's
    // PATH_LOCK; using a second, independent lock here would let a
    // set_var("AGENTFLARE_HOME_OVERRIDE") race a set_var("PATH") on another
    // thread — exactly the UB set_var is unsafe for.
    use agent_registry::detect::PATH_LOCK as GLOBAL_STATE_LOCK;

    pub(crate) fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("agentflare-test-home");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe {
            // SAFETY: GLOBAL_STATE_LOCK mutex serializes all env mutations;
            // no other thread can read or write AGENTFLARE_HOME_OVERRIDE concurrently.
            std::env::set_var("AGENTFLARE_HOME_OVERRIDE", &dir)
        };
        let result = f();
        unsafe {
            // SAFETY: GLOBAL_STATE_LOCK mutex serializes all env mutations.
            std::env::remove_var("AGENTFLARE_HOME_OVERRIDE")
        };
        result
    }

    pub(crate) fn with_temp_cwd<T>(f: impl FnOnce() -> T) -> T {
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("agentflare-test-cwd");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let result = f();
        std::env::set_current_dir(&original).unwrap();
        result
    }
}
