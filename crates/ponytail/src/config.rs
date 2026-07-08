use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_MODE: &str = "full";
pub const VALID_MODES: &[&str] = &[
    "off", "lite", "full", "ultra", "review", "audit", "debt", "gain", "help", "playbook",
];
pub const RUNTIME_MODES: &[&str] = &["off", "lite", "full", "ultra"];

pub fn normalize_mode(mode: &str) -> Option<&'static str> {
    let m = mode.trim().to_lowercase();
    RUNTIME_MODES.iter().find(|&&v| v == m).copied()
}

pub fn normalize_config_mode(mode: &str) -> Option<&'static str> {
    let m = mode.trim().to_lowercase();
    VALID_MODES.iter().find(|&&v| v == m).copied()
}

pub fn normalize_persisted_mode(mode: &str) -> Option<&'static str> {
    normalize_mode(mode).or_else(|| normalize_config_mode(mode))
}

/// Like `normalize_config_mode`, but also accepts user-defined custom skill
/// names (which are discovered at runtime, so they can't be `&'static str`).
pub fn normalize_extended_mode(mode: &str) -> Option<String> {
    let m = mode.trim().to_lowercase();
    normalize_config_mode(&m)
        .map(str::to_string)
        .or_else(|| crate::sub_skills::get_custom(&m).map(|_| m))
}

pub fn is_deactivation(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    let t = t.trim_end_matches(|c: char| c == '.' || c == '!' || c == '?' || c.is_whitespace());
    t == "stop ponytail" || t == "normal mode"
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agentflare")
        .join("ponytail")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

#[derive(Serialize, Deserialize, Default)]
struct ConfigFile {
    default_mode: Option<String>,
}

pub fn default_mode() -> String {
    if let Ok(val) = std::env::var("PONYTAIL_DEFAULT_MODE")
        && let Some(m) = normalize_extended_mode(&val)
    {
        return m;
    }
    if let Ok(data) = std::fs::read_to_string(config_path())
        && let Ok(cfg) = serde_json::from_str::<ConfigFile>(&data)
        && let Some(mode) = cfg.default_mode
        && let Some(m) = normalize_extended_mode(&mode)
    {
        return m;
    }
    DEFAULT_MODE.to_string()
}

pub fn set_default_mode(mode: &str) -> Result<(), String> {
    let normalized = normalize_extended_mode(mode).ok_or_else(|| format!("invalid mode: {mode}"))?;
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut cfg: ConfigFile = std::fs::read_to_string(config_path())
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or_default();
    cfg.default_mode = Some(normalized);
    let json = serde_json::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    std::fs::write(config_path(), json).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_valid_modes() {
        assert_eq!(normalize_mode("full"), Some("full"));
        assert_eq!(normalize_mode("off"), Some("off"));
        assert_eq!(normalize_mode("ULTRA"), Some("ultra"));
    }

    #[test]
    fn rejects_invalid_modes() {
        assert_eq!(normalize_mode("extreme"), None);
        assert_eq!(normalize_mode(""), None);
        assert_eq!(normalize_config_mode("review"), Some("review"));
        assert_eq!(normalize_mode("review"), None);
    }

    #[test]
    fn detects_deactivation() {
        assert!(is_deactivation("stop ponytail"));
        assert!(is_deactivation("normal mode"));
        assert!(is_deactivation("Normal Mode."));
        assert!(!is_deactivation("add a normal mode toggle"));
    }

    #[test]
    fn defaults_to_full() {
        unsafe { std::env::remove_var("PONYTAIL_DEFAULT_MODE") };
        assert_eq!(default_mode(), "full");
    }

    #[test]
    fn reads_env_var() {
        unsafe { std::env::set_var("PONYTAIL_DEFAULT_MODE", "lite") };
        assert_eq!(default_mode(), "lite");
        unsafe { std::env::remove_var("PONYTAIL_DEFAULT_MODE") };
    }
}

#[test]
fn roundtrip_default_mode() {
    let prev = default_mode();
    set_default_mode("ultra").unwrap();
    assert_eq!(default_mode(), "ultra");
    set_default_mode(&prev).unwrap();
}
