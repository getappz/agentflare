use std::path::PathBuf;

/// Resolves the user's home directory, honoring `FLARE_VAULT_HOME_OVERRIDE`
/// so this crate's own tests (and an embedding application's tests) never
/// touch the real home directory. Every home-directory lookup in this crate
/// must go through here rather than calling `dirs::home_dir()` directly.
pub fn home_dir() -> PathBuf {
    if let Ok(p) = std::env::var("FLARE_VAULT_HOME_OVERRIDE") {
        return PathBuf::from(p);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    // Serializes all FLARE_VAULT_HOME_OVERRIDE mutation across this test
    // binary's threads -- cargo test runs tests in parallel by default, and
    // std::env::set_var on a shared process-global var races otherwise.
    static HOME_OVERRIDE_LOCK: Mutex<()> = Mutex::new(());

    struct ResetHomeOverrideOnDrop(Option<std::ffi::OsString>);
    impl Drop for ResetHomeOverrideOnDrop {
        fn drop(&mut self) {
            unsafe {
                // SAFETY: still under HOME_OVERRIDE_LOCK for the duration of
                // this guard's life.
                if let Some(value) = &self.0 {
                    std::env::set_var("FLARE_VAULT_HOME_OVERRIDE", value);
                } else {
                    std::env::remove_var("FLARE_VAULT_HOME_OVERRIDE");
                }
            }
        }
    }

    pub(crate) fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = HOME_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("FLARE_VAULT_HOME_OVERRIDE");
        unsafe {
            // SAFETY: HOME_OVERRIDE_LOCK serializes all env mutation of this
            // var; no other thread can read or write it concurrently.
            std::env::set_var("FLARE_VAULT_HOME_OVERRIDE", dir.path());
        }
        let _reset = ResetHomeOverrideOnDrop(previous);
        f()
    }

    #[test]
    fn home_override_survives_temp_home_scope() {
        const CHILD: &str = "FLARE_VAULT_HOME_RESTORE_CHILD";
        const HOME: &str = "FLARE_VAULT_HOME_OVERRIDE";
        if std::env::var_os(CHILD).is_none() {
            for inherited in [true, false] {
                let mut child = std::process::Command::new(std::env::current_exe().unwrap());
                child
                    .args([
                        "--exact",
                        "paths::test_support::home_override_survives_temp_home_scope",
                    ])
                    .env(CHILD, "1");
                if inherited {
                    child.env(HOME, std::env::temp_dir().join("vault-outer-test-home"));
                } else {
                    child.env_remove(HOME);
                }
                let output = child.output().unwrap();
                assert!(
                    output.status.success(),
                    "inherited={inherited}: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        }
        let before = std::env::var_os(HOME);
        for unwind in [false, true] {
            let result = std::panic::catch_unwind(|| {
                with_temp_home(|| {
                    assert_ne!(std::env::var_os(HOME), before);
                    assert!(super::home_dir().is_dir());
                    assert!(!unwind, "exercise panic cleanup");
                })
            });
            assert_eq!(result.is_err(), unwind);
            assert_eq!(std::env::var_os(HOME), before);
        }
    }
}
