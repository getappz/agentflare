//! Configuration: ~/.config/flared/config.toml, every field optional with
//! safe defaults. A missing or unreadable config never stops the daemon.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::policy::OrphanRule;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryCheckConfig {
    /// Registry format. Only "lean-ctx" is understood today.
    pub kind: String,
    pub path: PathBuf,
    /// Substring the live process name must contain to count as the
    /// registered process (case-insensitive).
    pub expected_exe: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    pub light_interval_secs: u64,
    pub deep_interval_secs: u64,
    pub identity_tolerance_secs: u64,
    /// Extra never-touch patterns on top of the protected buckets.
    pub protect_patterns: Vec<String>,
    pub agent_patterns: Vec<String>,
    pub browser_patterns: Vec<String>,
    pub terminal_patterns: Vec<String>,
    pub desktop_patterns: Vec<String>,
    pub build_patterns: Vec<String>,
    pub orphan_rules: Vec<OrphanRule>,
    pub registries: Vec<RegistryCheckConfig>,
}

impl Default for Config {
    fn default() -> Self {
        fn strings(items: &[&str]) -> Vec<String> {
            items.iter().map(|s| s.to_string()).collect()
        }
        let lean_ctx_registry = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".local")
            .join("share")
            .join("lean-ctx")
            .join("agents")
            .join("registry.json");
        Config {
            port: 35273,
            light_interval_secs: 60,
            deep_interval_secs: 300,
            identity_tolerance_secs: 5,
            protect_patterns: Vec::new(),
            agent_patterns: strings(&[
                "claude", "codex", "aider", "cursor", "copilot", "gemini", "goose", "mcp",
                "lean-ctx", "agentflare", "native-host", "opencode",
            ]),
            browser_patterns: strings(&[
                "chrome", "msedge", "firefox", "brave", "opera", "vivaldi", "safari",
            ]),
            terminal_patterns: strings(&[
                "windowsterminal", "conhost", "cmd.exe", "powershell", "pwsh", "bash", "zsh",
                "fish", "wezterm", "alacritty", "kitty", "iterm", "tmux",
            ]),
            desktop_patterns: strings(&[
                "explorer.exe", "dwm.exe", "finder", "dock", "gnome-shell", "kwin", "plasmashell",
            ]),
            build_patterns: strings(&[
                "cargo", "rustc", "sccache", "msbuild", "cl.exe", "link.exe", "gcc", "clang",
                "tsc", "vite", "webpack", "gradle", "javac", "go.exe",
            ]),
            orphan_rules: vec![OrphanRule {
                name_pattern: "(?i)mcp|native-host".into(),
                require_dead_parent: true,
                min_age_secs: 3600,
            }],
            registries: vec![RegistryCheckConfig {
                kind: "lean-ctx".into(),
                path: lean_ctx_registry,
                expected_exe: "lean-ctx".into(),
            }],
        }
    }
}

impl Config {
    /// Load from `path` (or the default location when None). Missing file or
    /// parse error -> defaults, with a warning for the latter.
    pub fn load(path: Option<&Path>) -> Config {
        let path = path.map(Path::to_path_buf).unwrap_or_else(default_config_path);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => return Config::default(),
        };
        match toml::from_str(&text) {
            Ok(cfg) => cfg,
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "unparseable config, using defaults");
                Config::default()
            }
        }
    }
}

pub fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("flared")
        .join("config.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn defaults_are_sane() {
        let cfg = Config::default();
        assert_eq!(cfg.port, 35273);
        assert_eq!(cfg.light_interval_secs, 60);
        assert_eq!(cfg.deep_interval_secs, 300);
        assert!(cfg.identity_tolerance_secs >= 2);
        assert!(!cfg.agent_patterns.is_empty());
        assert!(!cfg.browser_patterns.is_empty());
        assert!(!cfg.terminal_patterns.is_empty());
        // lean-ctx registry check ships as a default janitor target.
        assert_eq!(cfg.registries.len(), 1);
        assert_eq!(cfg.registries[0].kind, "lean-ctx");
        assert_eq!(cfg.registries[0].expected_exe, "lean-ctx");
    }

    #[test]
    fn partial_toml_overrides_keep_other_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "port = 4444\n").unwrap();
        let cfg = Config::load(Some(&path));
        assert_eq!(cfg.port, 4444);
        assert_eq!(cfg.light_interval_secs, 60);
        assert!(!cfg.agent_patterns.is_empty());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let cfg = Config::load(Some(Path::new("Z:/definitely/not/here.toml")));
        assert_eq!(cfg.port, 35273);
    }

    #[test]
    fn unparseable_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "port = {{{{").unwrap();
        let cfg = Config::load(Some(&path));
        assert_eq!(cfg.port, 35273);
    }
}
