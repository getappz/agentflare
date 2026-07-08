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
