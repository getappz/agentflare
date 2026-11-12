// Home-directory resolution with an explicit test override. `dirs::home_dir()`
// resolves via the OS directly on Windows (SHGetKnownFolderPath) and ignores
// HOME/USERPROFILE env var overrides — learned the hard way when a
// "sandboxed" test run wrote real changes to a live ~/.claude/settings.json.
// AGENTFLARE_HOME_OVERRIDE is agentflare's own escape hatch for tests/CI.
use std::path::PathBuf;

pub mod paths;

/// Fallible twin of `home()` below, for the rare caller that needs to
/// degrade gracefully (e.g. a sandbox wrapper that just skips a bind mount)
/// instead of panicking when no home directory can be resolved at all.
#[must_use]
pub fn try_home() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AGENTFLARE_HOME_OVERRIDE") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir()
}

/// # Panics
///
/// Panics if the home directory cannot be resolved (`dirs::home_dir()`
/// returns `None`).
#[must_use]
pub fn home() -> PathBuf {
    try_home().expect("home directory not found")
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

/// `~/.agentflare` — agentflare's own per-user data dir (`agentflare.db`,
/// `store.db`, `backend.db`, artifacts, staging, shims, config.toml, ...).
/// The one resolver every per-user `.agentflare` consumer should share —
/// `home()` above already honors `AGENTFLARE_HOME_OVERRIDE`, unlike
/// `dirs::home_dir()`/`$HOME` called directly, which either ignore the
/// override or, on Windows, may not even be set (`HOME` isn't `USERPROFILE`).
#[must_use]
pub fn agentflare_dir() -> PathBuf {
    home().join(".agentflare")
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

    /// Owns an isolated home and the shared environment lock across a test scope.
    /// Do not nest this guard with another home/PATH fixture holding that lock.
    #[must_use]
    pub struct TempHome {
        previous_home: Option<std::ffi::OsString>,
        previous_vault: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            unsafe {
                // SAFETY: the guard releases GLOBAL_STATE_LOCK after restoration.
                for (name, previous) in [
                    ("AGENTFLARE_HOME_OVERRIDE", &self.previous_home),
                    ("FLARE_VAULT_HOME_OVERRIDE", &self.previous_vault),
                ] {
                    if let Some(value) = previous {
                        std::env::set_var(name, value);
                    } else {
                        std::env::remove_var(name);
                    }
                }
            }
        }
    }

    /// # Panics
    ///
    /// Panics if a temporary directory cannot be created.
    pub fn temp_home() -> TempHome {
        let guard = GLOBAL_STATE_LOCK
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
        let previous_home = std::env::var_os("AGENTFLARE_HOME_OVERRIDE");
        let previous_vault = std::env::var_os("FLARE_VAULT_HOME_OVERRIDE");
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
        TempHome {
            previous_home,
            previous_vault,
            _dir: dir,
            _guard: guard,
        }
    }

    pub fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _home = temp_home();
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
    fn with_temp_home_restores_overrides_after_return_and_unwind() {
        const CHILD: &str = "AGENTFLARE_HOME_RESTORE_TEST_CHILD";
        const HOME: &str = "AGENTFLARE_HOME_OVERRIDE";
        const VAULT: &str = "FLARE_VAULT_HOME_OVERRIDE";
        if std::env::var_os(CHILD).is_none() {
            let outer = tempfile::tempdir().unwrap();
            for (inherited_home, inherited_vault) in
                [(true, true), (true, false), (false, true), (false, false)]
            {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child
                    .args([
                        "--exact",
                        "tests::with_temp_home_restores_overrides_after_return_and_unwind",
                    ])
                    .env(CHILD, "1");
                if inherited_home {
                    child.env(HOME, outer.path().join("agentflare"));
                } else {
                    child.env_remove(HOME);
                }
                if inherited_vault {
                    child.env(VAULT, outer.path().join("vault"));
                } else {
                    child.env_remove(VAULT);
                }
                let output = child.output().unwrap();
                assert!(
                    output.status.success(),
                    "home={inherited_home}, vault={inherited_vault}: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        }
        let before = (std::env::var_os(HOME), std::env::var_os(VAULT));
        for unwind in [false, true] {
            let result = std::panic::catch_unwind(|| {
                with_temp_home(|| {
                    let scoped = std::env::var_os(HOME).unwrap();
                    assert_eq!(std::env::var_os(VAULT), Some(scoped.clone()));
                    assert_ne!(Some(scoped), before.0);
                    assert!(super::home().is_dir());
                    assert!(!unwind, "exercise panic cleanup");
                    42
                })
            });
            assert_eq!(result.is_err(), unwind);
            if let Ok(value) = result {
                assert_eq!(value, 42);
            }
            assert_eq!((std::env::var_os(HOME), std::env::var_os(VAULT)), before);
        }
    }

    #[test]
    fn config_dir_is_namespaced_under_agentflare() {
        let dir = super::config_dir("flare-code");
        assert!(dir.ends_with("agentflare/flare-code"));
    }
}
