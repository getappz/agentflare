// Single JSON state blob, host-neutral (~/.leanstack/), shared across
// whichever agents this machine has run `leanstack init`/hooks for.
use crate::paths::home;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_true() -> bool {
    true
}

pub fn state_dir() -> PathBuf {
    home().join(".leanstack")
}

pub fn state_path() -> PathBuf {
    state_dir().join("state.json")
}

pub fn load() -> State {
    fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| State { active: true })
}

pub fn save(state: &State) {
    let _ = fs::create_dir_all(state_dir());
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = fs::write(state_path(), json + "\n");
    }
}
