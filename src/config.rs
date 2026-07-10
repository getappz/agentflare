// User-editable feature toggles, distinct from state.rs's ephemeral runtime
// state. Lives under ~/.agentflare/ (via paths::home(), not ponytail's
// dirs::config_dir() — that path ignores AGENTFLARE_HOME_OVERRIDE and would
// make tests touch the real ~/.config/agentflare on disk).
use crate::paths::home;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
pub struct Config {
    /// Fire a memory handoff when a Claude Code session ends (`/exit`,
    /// closing the terminal, etc.) via `hook session-end`.
    #[serde(default = "default_true")]
    pub handoff_on_session_end: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { handoff_on_session_end: true }
    }
}

fn default_true() -> bool {
    true
}

pub fn config_path() -> PathBuf {
    home().join(".agentflare").join("config.jsonc")
}

/// ponytail: full-line `//` comments only (inline `// ...` after JSON on the
/// same line is not stripped) — add a real jsonc parser if that's needed.
fn strip_line_comments(s: &str) -> String {
    s.lines()
        .map(|line| if line.trim_start().starts_with("//") { "" } else { line })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn load() -> Config {
    std::fs::read_to_string(config_path())
        .ok()
        .map(|s| strip_line_comments(&s))
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(cfg: &Config) -> std::io::Result<()> {
    let dir = home().join(".agentflare");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(config_path(), serde_json::to_string_pretty(cfg).unwrap() + "\n")
}

pub fn handoff_on_session_end() -> bool {
    load().handoff_on_session_end
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn defaults_to_enabled_when_no_config_file() {
        with_temp_home(|| {
            assert!(handoff_on_session_end());
        });
    }

    #[test]
    fn save_then_load_roundtrips() {
        with_temp_home(|| {
            save(&Config { handoff_on_session_end: false }).unwrap();
            assert!(!handoff_on_session_end());
        });
    }

    #[test]
    fn ignores_full_line_comments() {
        with_temp_home(|| {
            std::fs::create_dir_all(home().join(".agentflare")).unwrap();
            std::fs::write(
                config_path(),
                "{\n  // disable handoff on exit\n  \"handoff_on_session_end\": false\n}\n",
            )
            .unwrap();
            assert!(!handoff_on_session_end());
        });
    }

    #[test]
    fn falls_back_to_default_on_corrupt_file() {
        with_temp_home(|| {
            std::fs::create_dir_all(home().join(".agentflare")).unwrap();
            std::fs::write(config_path(), "not json").unwrap();
            assert!(handoff_on_session_end());
        });
    }
}
