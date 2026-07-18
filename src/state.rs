// Host-neutral local state (~/.agentflare/), shared across whichever agents
// this machine has run `agentflare init`/hooks for. Backed by
// `agentflare-store`'s kv table; a legacy `state.json` (the pre-store
// on-disk format) is imported once, in place, the first time this runs
// against a store that has neither key yet.
use crate::paths::home;
pub use agent_registry::VersionCacheEntry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

const ACTIVE_KEY: &str = "active";
const VERSION_CACHE_KEY: &str = "version_cache";

fn default_state() -> State {
    State {
        active: true,
        ..Default::default()
    }
}

pub fn load() -> State {
    let store = match crate::store::open() {
        Ok(store) => store,
        Err(e) => {
            eprintln!("[agentflare] warning: failed to open store: {e}");
            return default_state();
        }
    };

    // One-time bridge for installs that predate the store: if neither key
    // is present yet, but a legacy state.json is on disk, import it.
    // migrate_state_json flattens state.json's top-level keys ("active",
    // "version_cache") straight into these same kv keys, and records its
    // own marker so it never re-runs. A parse failure on a corrupt legacy
    // file is swallowed here -- the kv reads below then fall through to
    // defaults, same as the old file-based load() did on corrupt JSON.
    let has_active = store.kv_get(ACTIVE_KEY).ok().flatten().is_some();
    let has_version_cache = store.kv_get(VERSION_CACHE_KEY).ok().flatten().is_some();
    if !has_active && !has_version_cache {
        let legacy_path = state_path();
        if legacy_path.exists() {
            let _ = agentflare_store::migrate::migrate_state_json(&store, &legacy_path);
        }
    }

    let active = store
        .kv_get(ACTIVE_KEY)
        .ok()
        .flatten()
        .and_then(|entry| serde_json::from_slice(&entry.value).ok())
        .unwrap_or(true);
    let version_cache = store
        .kv_get(VERSION_CACHE_KEY)
        .ok()
        .flatten()
        .and_then(|entry| serde_json::from_slice(&entry.value).ok())
        .unwrap_or_default();

    State {
        active,
        version_cache,
    }
}

pub fn save(state: &State) {
    let store = match crate::store::open() {
        Ok(store) => store,
        Err(e) => {
            eprintln!("[agentflare] warning: failed to open store: {e}");
            return;
        }
    };
    match serde_json::to_vec(&state.active) {
        Ok(bytes) => {
            if let Err(e) = store.kv_set(ACTIVE_KEY, &bytes) {
                eprintln!("[agentflare] warning: failed to persist state: {e}");
            }
        }
        Err(e) => eprintln!("[agentflare] warning: failed to serialize state: {e}"),
    }
    match serde_json::to_vec(&state.version_cache) {
        Ok(bytes) => {
            if let Err(e) = store.kv_set(VERSION_CACHE_KEY, &bytes) {
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
    use std::fs;

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
    fn load_falls_back_to_default_on_corrupt_legacy_file() {
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

    #[test]
    fn legacy_state_file_is_migrated_exactly_once() {
        with_temp_home(|| {
            fs::create_dir_all(state_dir()).unwrap();
            fs::write(state_path(), r#"{"active": false}"#).unwrap();

            let first = load();
            assert!(!first.active);

            // Mutate the legacy file after the first load -- since
            // migration already ran (kv keys now present), the second
            // load must read from the store, not re-import the file.
            fs::write(state_path(), r#"{"active": true}"#).unwrap();
            let second = load();
            assert!(!second.active);
        });
    }
}
