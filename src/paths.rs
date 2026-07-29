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
    use std::path::PathBuf;

    // Removes AGENTFLARE_HOME_OVERRIDE on drop -- including on unwind, so a
    // panicking assertion inside `f()` can't leave the override set for
    // whatever test runs next on another thread once GLOBAL_STATE_LOCK is
    // released (poisoned-mutex recovery only protects the lock itself, not
    // env state a previous holder forgot to restore).
    struct ResetHomeOverrideOnDrop;
    impl Drop for ResetHomeOverrideOnDrop {
        fn drop(&mut self) {
            unsafe {
                // SAFETY: still under GLOBAL_STATE_LOCK for the duration of
                // this guard's life.
                std::env::remove_var("AGENTFLARE_HOME_OVERRIDE");
            }
        }
    }

    pub(crate) fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // A fresh, uniquely-named directory per call -- not a fixed shared
        // name -- so a previous call's leftover file handle (e.g. a SQLite
        // -wal/-shm file Windows hasn't released yet) can never leak into
        // the next call even if that previous directory hasn't finished
        // being cleaned up. See git history for the shared-fixed-name bug
        // this replaced (state.rs/vent::capture.rs tests intermittently
        // observed each other's persisted state under parallel execution).
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            // SAFETY: GLOBAL_STATE_LOCK mutex serializes all env mutations;
            // no other thread can read or write AGENTFLARE_HOME_OVERRIDE concurrently.
            std::env::set_var("AGENTFLARE_HOME_OVERRIDE", dir.path())
        };
        let _reset = ResetHomeOverrideOnDrop;
        f()
    }

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

#[cfg(test)]
mod tests {
    use super::test_support::with_temp_home;

    // Regression test for a real Windows CI flake (state::tests::* and
    // vent::capture::tests::* intermittently observed each other's
    // persisted state under `cargo test --workspace`'s default parallel
    // runner): with_temp_home used a single fixed directory name shared by
    // every call, so a previous call's file (left behind if e.g. Windows
    // hadn't yet released a SQLite -wal/-shm handle) could still be present
    // when the next call's directory was supposed to be empty.
    #[test]
    fn with_temp_home_never_sees_a_previous_calls_leftover_file() {
        for i in 0..20 {
            with_temp_home(|| {
                let marker = super::home().join("marker.txt");
                assert!(
                    !marker.exists(),
                    "iteration {i}: found a marker file left behind by a previous with_temp_home call at {}",
                    super::home().display()
                );
                std::fs::write(&marker, "left behind on purpose").unwrap();
            });
        }
    }

    // Same check under real thread contention -- GLOBAL_STATE_LOCK forces
    // these to run one at a time, but back-to-back-under-contention is
    // exactly the timing the original shared-fixed-directory bug needed to
    // show up under Windows' delayed file-handle release.
    #[test]
    fn with_temp_home_isolates_calls_under_thread_contention() {
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..20 {
                        with_temp_home(|| {
                            let marker = super::home().join("marker.txt");
                            assert!(
                                !marker.exists(),
                                "found a marker file left behind by another with_temp_home call at {}",
                                super::home().display()
                            );
                            std::fs::write(&marker, "left behind on purpose").unwrap();
                        });
                    }
                });
            }
        });
    }

    #[test]
    fn with_temp_home_clears_the_override_env_var_after_returning() {
        with_temp_home(|| {});
        // Read the var only while holding the same lock with_temp_home uses to
        // serialize all env mutation in this binary. Without it, this check
        // races a concurrent thread's own (correctly scoped) with_temp_home
        // call: that thread may be transiently holding the var set for its own
        // closure at the exact moment this assertion reads it, which isn't a
        // leak on our part — it's a var we were never entitled to observe.
        let _guard = agent_registry::detect::PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert!(
            std::env::var("AGENTFLARE_HOME_OVERRIDE").is_err(),
            "AGENTFLARE_HOME_OVERRIDE must not remain set once with_temp_home returns"
        );
    }
}
