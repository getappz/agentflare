use std::io;
use std::path::PathBuf;

/// Test-only override so `flag_path`/`session_path` don't touch the real
/// on-disk state dir. `FLARE_CODE_STATE_DIR_OVERRIDE` takes precedence over
/// `dirs::state_dir()` when set, same mechanism as `config::config_dir()`'s
/// `FLARE_CODE_CONFIG_DIR_OVERRIDE`.
fn state_dir() -> PathBuf {
    if let Ok(p) = std::env::var("FLARE_CODE_STATE_DIR_OVERRIDE") {
        return PathBuf::from(p);
    }
    dirs::state_dir()
        .unwrap_or_else(|| dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")))
}

#[must_use]
pub fn flag_path() -> PathBuf {
    state_dir()
        .join("agentflare")
        .join("flare-code")
        .join("active")
}

#[must_use]
pub fn session_path() -> PathBuf {
    state_dir()
        .join("agentflare")
        .join("flare-code")
        .join("session-mode")
}

fn read_session() -> Option<String> {
    std::fs::read_to_string(session_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[must_use]
pub fn active_mode() -> Option<String> {
    read_session().or_else(|| {
        std::fs::read_to_string(flag_path())
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// # Errors
///
/// Returns `Err` if the flag directory can't be created or the flag file
/// can't be written.
pub fn set_active(mode: &str) -> io::Result<()> {
    let path = flag_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, mode)
}

/// # Errors
///
/// Returns `Err` if the session directory can't be created or the session
/// file can't be written.
pub fn set_session(mode: &str) -> io::Result<()> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, mode)
}

pub fn clear_session() {
    let _ = std::fs::remove_file(session_path());
}

pub fn clear_active() {
    let _ = std::fs::remove_file(flag_path());
    clear_session();
}

#[must_use]
pub fn active_scope() -> &'static str {
    if read_session().is_some() {
        "session"
    } else {
        "global"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Both tests read/write the same state files (`flag_path()` /
    // `session_path()`). `STATE_LOCK` serializes them within one process
    // (cargo test's default), but nextest runs each test as its own
    // process, where a process-local mutex protects nothing -- sibling
    // processes would race on the same real on-disk state dir otherwise.
    // `with_temp_state_dir` keys the override by process id so every
    // nextest process gets its own directory instead of touching the real
    // one, same fix as `config::with_temp_config_dir`.
    static STATE_LOCK: Mutex<()> = Mutex::new(());

    struct StateDirOverrideGuard;

    #[allow(unsafe_code)]
    impl Drop for StateDirOverrideGuard {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("FLARE_CODE_STATE_DIR_OVERRIDE") };
        }
    }

    #[allow(unsafe_code)]
    fn with_temp_state_dir<T>(f: impl FnOnce() -> T) -> T {
        let _guard = STATE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir =
            std::env::temp_dir().join(format!("flare-code-test-state-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        unsafe { std::env::set_var("FLARE_CODE_STATE_DIR_OVERRIDE", &dir) };
        let _override_guard = StateDirOverrideGuard;
        f()
    }

    #[test]
    fn roundtrip_active_mode() {
        with_temp_state_dir(|| {
            clear_active();
            assert_eq!(active_mode(), None);

            set_active("full").unwrap();
            assert_eq!(active_mode(), Some("full".to_string()));
            assert_eq!(active_scope(), "global");

            set_session("ultra").unwrap();
            assert_eq!(active_mode(), Some("ultra".to_string()));
            assert_eq!(active_scope(), "session");

            clear_session();
            assert_eq!(active_mode(), Some("full".to_string()));
            assert_eq!(active_scope(), "global");

            clear_active();
            assert_eq!(active_mode(), None);
        });
    }

    #[test]
    fn clear_nonexistent_is_noop() {
        with_temp_state_dir(|| {
            clear_active();
            clear_active();
            clear_session();
        });
    }
}
