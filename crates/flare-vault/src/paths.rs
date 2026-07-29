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

    struct ResetHomeOverrideOnDrop;
    impl Drop for ResetHomeOverrideOnDrop {
        fn drop(&mut self) {
            unsafe {
                // SAFETY: still under HOME_OVERRIDE_LOCK for the duration of
                // this guard's life.
                std::env::remove_var("FLARE_VAULT_HOME_OVERRIDE");
            }
        }
    }

    pub(crate) fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = HOME_OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        unsafe {
            // SAFETY: HOME_OVERRIDE_LOCK serializes all env mutation of this
            // var; no other thread can read or write it concurrently.
            std::env::set_var("FLARE_VAULT_HOME_OVERRIDE", dir.path());
        }
        let _reset = ResetHomeOverrideOnDrop;
        f()
    }
}
