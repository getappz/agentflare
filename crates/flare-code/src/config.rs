use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_MODE: &str = "full";
pub const VALID_MODES: &[&str] = &[
    "off",
    "lite",
    "full",
    "ultra",
    "review",
    "audit",
    "debt",
    "gain",
    "help",
    "playbook",
    "no-hallucination",
];
pub const RUNTIME_MODES: &[&str] = &["off", "lite", "full", "ultra"];

#[must_use]
pub fn normalize_mode(mode: &str) -> Option<&'static str> {
    let m = mode.trim().to_lowercase();
    RUNTIME_MODES.iter().find(|&&v| v == m).copied()
}

#[must_use]
pub fn normalize_config_mode(mode: &str) -> Option<&'static str> {
    let m = mode.trim().to_lowercase();
    VALID_MODES.iter().find(|&&v| v == m).copied()
}

#[must_use]
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

/// Serializes tests (in this module and `instructions.rs`) that mutate
/// process-global env vars (`CLAUDE_CONFIG_DIR`, `FLARE_CODE_DEFAULT_MODE`
/// and legacy `PONYTAIL_DEFAULT_MODE`) —
/// `cargo test` runs unit tests in parallel by default, so unguarded
/// `set_var`/`remove_var` calls can leak across assertions.
pub static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Compression/persona plugins known to conflict with flare-code's own style
/// guidance if both are active — e.g. caveman's terse-prose rules vs
/// flare-code's own output-shape rules.
const KNOWN_COMPRESSION_PLUGINS: &[&str] = &["caveman"];

fn claude_dir() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR").map_or_else(
        |_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".claude")
        },
        PathBuf::from,
    )
}

/// Scans `~/.claude/settings.json` (or `$CLAUDE_CONFIG_DIR/settings.json`)
/// for known compression/persona plugins so flare-code can add a
/// deconfliction note instead of silently contradicting them.
#[must_use]
pub fn detect_compression_plugins() -> Vec<&'static str> {
    let Ok(raw) = std::fs::read_to_string(claude_dir().join("settings.json")) else {
        return Vec::new();
    };
    let blob = raw.to_lowercase();
    KNOWN_COMPRESSION_PLUGINS
        .iter()
        .filter(|name| blob.contains(*name))
        .copied()
        .collect()
}

#[must_use]
pub fn is_deactivation(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    let t = t.trim_end_matches(|c: char| c == '.' || c == '!' || c == '?' || c.is_whitespace());
    t == "stop flare-code" || t == "stop ponytail" || t == "normal mode"
}

/// `dirs::config_dir()` resolves via the OS directly and ignores env-var
/// overrides — same footgun `paths::home()` documents in the root crate
/// (a "sandboxed" test run silently writing to the real ~/.config).
/// `FLARE_CODE_CONFIG_DIR_OVERRIDE` (and legacy `PONYTAIL_CONFIG_DIR_OVERRIDE`)
/// is this crate's own escape hatch for tests.
#[must_use]
pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("FLARE_CODE_CONFIG_DIR_OVERRIDE")
        .or_else(|_| std::env::var("PONYTAIL_CONFIG_DIR_OVERRIDE"))
    {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agentflare")
        .join("flare-code")
}

/// Test-only isolation for `config_dir()` — without this, tests that read
/// or write the persisted default mode (`default_mode`/`set_default_mode`)
/// hit the real on-disk config file and can race each other under cargo's
/// parallel test runner (see the `defaults_to_full` vs `roundtrip_default_mode`
/// flake this was added to fix).
#[cfg(test)]
struct ConfigDirOverrideGuard;

#[cfg(test)]
#[allow(unsafe_code)]
impl Drop for ConfigDirOverrideGuard {
    fn drop(&mut self) {
        unsafe { std::env::remove_var("FLARE_CODE_CONFIG_DIR_OVERRIDE") };
        unsafe { std::env::remove_var("PONYTAIL_CONFIG_DIR_OVERRIDE") };
    }
}

#[cfg(test)]
#[allow(unsafe_code)]
fn with_temp_config_dir<T>(f: impl FnOnce() -> T) -> T {
    let _guard = ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = std::env::temp_dir().join("flare-code-test-config-dir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    unsafe { std::env::set_var("FLARE_CODE_CONFIG_DIR_OVERRIDE", &dir) };
    // Dropped (restoring the env var) even if `f()` panics — a bare
    // remove_var() call after f() would be skipped on panic, leaking the
    // override process-wide for the rest of the test binary.
    let _override_guard = ConfigDirOverrideGuard;
    f()
}

#[must_use]
pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

#[derive(Serialize, Deserialize, Default)]
struct ConfigFile {
    default_mode: Option<String>,
}

#[must_use]
pub fn default_mode() -> String {
    if let Ok(val) =
        std::env::var("FLARE_CODE_DEFAULT_MODE").or_else(|_| std::env::var("PONYTAIL_DEFAULT_MODE"))
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
    let normalized =
        normalize_extended_mode(mode).ok_or_else(|| format!("invalid mode: {mode}"))?;
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
    fn no_compression_plugins_when_settings_missing() {
        let _guard = ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", "/nonexistent/flare-code-test-dir") };
        assert!(detect_compression_plugins().is_empty());
        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };
    }

    #[test]
    fn detects_caveman_in_settings_json() {
        let _guard = ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = std::env::temp_dir().join("flare-code_test_compression_conflict");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), r#"{"plugins": ["caveman"]}"#).unwrap();
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &dir) };
        assert_eq!(detect_compression_plugins(), vec!["caveman"]);
        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };
        std::fs::remove_dir_all(&dir).ok();
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
        with_temp_config_dir(|| {
            unsafe { std::env::remove_var("FLARE_CODE_DEFAULT_MODE") };
            unsafe { std::env::remove_var("PONYTAIL_DEFAULT_MODE") };
            assert_eq!(default_mode(), "full");
        });
    }

    #[test]
    fn reads_env_var() {
        with_temp_config_dir(|| {
            unsafe { std::env::set_var("FLARE_CODE_DEFAULT_MODE", "lite") };
            assert_eq!(default_mode(), "lite");
            unsafe { std::env::remove_var("FLARE_CODE_DEFAULT_MODE") };
        });
    }
}

#[test]
#[allow(unsafe_code)]
fn roundtrip_default_mode() {
    with_temp_config_dir(|| {
        unsafe { std::env::remove_var("FLARE_CODE_DEFAULT_MODE") };
        unsafe { std::env::remove_var("PONYTAIL_DEFAULT_MODE") };
        let prev = default_mode();
        set_default_mode("ultra").unwrap();
        assert_eq!(default_mode(), "ultra");
        set_default_mode(&prev).unwrap();
    });
}
