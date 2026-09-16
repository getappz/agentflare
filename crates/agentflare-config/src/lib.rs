// Home-directory resolution with an explicit test override. `dirs::home_dir()`
// resolves via the OS directly on Windows (SHGetKnownFolderPath) and ignores
// HOME/USERPROFILE env var overrides — learned the hard way when a
// "sandboxed" test run wrote real changes to a live ~/.claude/settings.json.
// AGENTFLARE_HOME_OVERRIDE is agentflare's own escape hatch for tests/CI.
use std::path::PathBuf;

/// # Panics
///
/// Panics if the home directory cannot be resolved (`dirs::home_dir()`
/// returns `None`).
#[must_use]
pub fn home() -> PathBuf {
    if let Ok(p) = std::env::var("AGENTFLARE_HOME_OVERRIDE") {
        return PathBuf::from(p);
    }
    dirs::home_dir().expect("home directory not found")
}

/// `~/.config/agentflare/<namespace>` (or the OS config dir equivalent) —
/// e.g. `config_dir("flare-code")` -> `~/.config/agentflare/flare-code`.
#[must_use]
pub fn config_dir(namespace: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agentflare")
        .join(namespace)
}

// Shared by every dependent crate's tests: AGENTFLARE_HOME_OVERRIDE and cwd
// are process-global, so tests that touch either must run serialized against
// each other or they'll stomp on one another under cargo's default
// parallel test runner. Not `#[cfg(test)]`-gated because a dependent crate's
// own test build doesn't inherit this crate's `cfg(test)` — it needs
// `test_support` present in the normal build to link against it.
pub mod test_support {
    // One process-wide lock for ALL env mutation in this test binary.
    // src/agents.rs already serializes PATH edits on agent_registry's
    // PATH_LOCK; using a second, independent lock here would let a
    // set_var("AGENTFLARE_HOME_OVERRIDE") race a set_var("PATH") on another
    // thread — exactly the UB set_var is unsafe for.
    use agent_registry::detect::PATH_LOCK as GLOBAL_STATE_LOCK;

    // Removes AGENTFLARE_HOME_OVERRIDE/FLARE_VAULT_HOME_OVERRIDE on drop --
    // including on unwind, so a panicking assertion inside `f()` can't leave
    // the override set for whatever test runs next on another thread once
    // GLOBAL_STATE_LOCK is released (poisoned-mutex recovery only protects
    // the lock itself, not env state a previous holder forgot to restore).
    struct ResetHomeOverrideOnDrop;
    impl Drop for ResetHomeOverrideOnDrop {
        fn drop(&mut self) {
            unsafe {
                // SAFETY: still under GLOBAL_STATE_LOCK for the duration of
                // this guard's life.
                std::env::remove_var("AGENTFLARE_HOME_OVERRIDE");
                std::env::remove_var("FLARE_VAULT_HOME_OVERRIDE");
            }
        }
    }

    /// # Panics
    ///
    /// Panics if a temporary directory cannot be created.
    pub fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = GLOBAL_STATE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            // no other thread can read or write these vars concurrently.
            // FLARE_VAULT_HOME_OVERRIDE is flare-vault's own escape hatch
            // (crates/flare-vault/src/paths.rs) -- set alongside
            // AGENTFLARE_HOME_OVERRIDE so vault.rs tests get one isolated
            // home for both the vault file and flare-vault's session cache.
            std::env::set_var("AGENTFLARE_HOME_OVERRIDE", dir.path());
            std::env::set_var("FLARE_VAULT_HOME_OVERRIDE", dir.path());
        };
        let _reset = ResetHomeOverrideOnDrop;
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
        assert!(
            std::env::var("FLARE_VAULT_HOME_OVERRIDE").is_err(),
            "FLARE_VAULT_HOME_OVERRIDE must not remain set once with_temp_home returns"
        );
    }

    #[test]
    fn config_dir_is_namespaced_under_agentflare() {
        let dir = super::config_dir("flare-code");
        assert!(dir.ends_with("agentflare/flare-code"));
    }
}
