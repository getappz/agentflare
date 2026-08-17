//! Session-scoped PM-mode flag: the same file-based "stays active until
//! explicitly turned off" pattern flare-code uses for its own session mode
//! (crates/flare-code/src/state.rs's session_path/set_session/clear_session),
//! adopted here so PM mode survives context compaction/long sessions without
//! depending on the model remembering it — hook.rs's UserPromptSubmit
//! handler re-injects a reminder every turn while the flag is set.

use crate::paths::home;
use std::path::PathBuf;

fn flag_path() -> PathBuf {
    home().join(".agentflare").join("pm-mode").join("session")
}

#[must_use]
pub fn is_active() -> bool {
    flag_path().is_file()
}

pub fn set_active() -> std::io::Result<()> {
    let path = flag_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, "on")
}

pub fn clear_active() {
    let _ = std::fs::remove_file(flag_path());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn roundtrip_active_flag() {
        with_temp_home(|| {
            assert!(!is_active());
            set_active().unwrap();
            assert!(is_active());
            clear_active();
            assert!(!is_active());
        });
    }

    #[test]
    fn clear_nonexistent_is_noop() {
        with_temp_home(|| {
            clear_active();
            clear_active();
        });
    }
}
