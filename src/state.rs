// Single JSON state blob, host-neutral (~/.agentflare/), shared across
// whichever agents this machine has run `agentflare init`/hooks for.
use crate::paths::home;
pub use agent_registry::VersionCacheEntry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    #[serde(default = "default_true")]
    pub active: bool,
    /// `agentflare agents` version-resolution cache, keyed by `Agent::as_str()`.
    #[serde(default)]
    pub version_cache: HashMap<String, VersionCacheEntry>,
}

fn default_true() -> bool {
    true
}

pub fn state_dir() -> PathBuf {
    home().join(".agentflare")
}

pub fn state_path() -> PathBuf {
    state_dir().join("state.json")
}

pub fn load() -> State {
    fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| State {
            active: true,
            ..Default::default()
        })
}

pub fn save(state: &State) {
    if let Err(e) = fs::create_dir_all(state_dir()) {
        eprintln!("[agentflare] warning: failed to create state dir: {e}");
        return;
    }
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            if let Err(e) = fs::write(state_path(), json + "\n") {
                eprintln!("[agentflare] warning: failed to persist state: {e}");
            }
        }
        Err(e) => eprintln!("[agentflare] warning: failed to serialize state: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn load_defaults_to_active_when_no_state_file() {
        with_temp_home(|| {
            assert!(load().active);
        });
    }

    #[test]
    fn save_then_load_roundtrips() {
        with_temp_home(|| {
            save(&State {
                active: false,
                ..Default::default()
            });
            assert!(!load().active);
        });
    }

    #[test]
    fn load_falls_back_to_default_on_corrupt_file() {
        with_temp_home(|| {
            fs::create_dir_all(state_dir()).unwrap();
            fs::write(state_path(), "not json").unwrap();
            assert!(load().active);
        });
    }

    #[test]
    fn version_cache_defaults_to_empty_when_absent_from_state_file() {
        with_temp_home(|| {
            assert!(load().version_cache.is_empty());
        });
    }

    #[test]
    fn version_cache_roundtrips_through_save_and_load() {
        with_temp_home(|| {
            let mut s = load();
            s.version_cache.insert(
                "claude-code".to_string(),
                VersionCacheEntry {
                    binary_path: "/usr/local/bin/claude".to_string(),
                    mtime: 1_700_000_000,
                    version: "1.2.3".to_string(),
                },
            );
            save(&s);

            let reloaded = load();
            let entry = reloaded.version_cache.get("claude-code").unwrap();
            assert_eq!(entry.binary_path, "/usr/local/bin/claude");
            assert_eq!(entry.mtime, 1_700_000_000);
            assert_eq!(entry.version, "1.2.3");
        });
    }

    #[test]
    fn old_state_file_without_version_cache_field_still_loads() {
        with_temp_home(|| {
            fs::create_dir_all(state_dir()).unwrap();
            fs::write(state_path(), r#"{"active": false}"#).unwrap();
            let s = load();
            assert!(!s.active);
            assert!(s.version_cache.is_empty());
        });
    }
}
