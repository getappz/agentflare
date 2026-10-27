use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct InsightsConfig {
    pub data_dir: PathBuf,
    pub db_path: PathBuf,
    pub retention_days: u32,
    pub sources: HashMap<String, PathBuf>,
    pub pricing: PricingTable,
}

impl Default for InsightsConfig {
    fn default() -> Self {
        let data_dir = dirs_data_dir();
        Self {
            db_path: data_dir.join("observatory.db"),
            data_dir,
            retention_days: 30,
            sources: default_sources(),
            pricing: PricingTable::default(),
        }
    }
}

fn dirs_data_dir() -> PathBuf {
    if let Some(dir) = dirs::data_dir() {
        dir.join("agentflare").join("insights")
    } else {
        PathBuf::from(".agentflare/insights")
    }
}

/// Home for source discovery. Prefer `$HOME`, then `$USERPROFILE` (Windows),
/// then `dirs::home_dir()` — never fall back to cwd `.`, which made Windows
/// syncs miss `~\.local\share\opencode\opencode.db`.
pub fn resolve_home() -> PathBuf {
    std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USERPROFILE").ok().filter(|s| !s.is_empty()))
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// OpenCode session DB path, mirroring opencode's own data-dir resolution.
pub fn opencode_db_path(home: &std::path::Path) -> PathBuf {
    if let Ok(dir) = std::env::var("OPENCODE_DATA_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("opencode.db");
        }
    }
    if let Ok(dir) = std::env::var("XDG_DATA_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join("opencode").join("opencode.db");
        }
    }
    home.join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db")
}

fn default_sources() -> HashMap<String, PathBuf> {
    let h = resolve_home();
    let mut m = HashMap::new();
    m.insert("claude_code".into(), h.join(".claude").join("projects"));
    m.insert("codex".into(), h.join(".codex").join("sessions"));
    m.insert("opencode".into(), opencode_db_path(&h));
    // Cursor has no ingest adapter yet (#321) — omit path mapping so `doctor`
    // does not list a dead source. Gemini/Copilot stay as reserved path stubs.
    m.insert("gemini".into(), h.join(".gemini"));
    m.insert("copilot".into(), h.join(".copilot"));
    // env overrides
    for (key, env) in [
        ("claude_code", "CLAUDE_PROJECTS_DIR"),
        ("codex", "CODEX_SESSIONS_DIR"),
        ("opencode", "OPENCODE_DIR"),
        ("gemini", "GEMINI_DIR"),
    ] {
        if let Ok(v) = std::env::var(env) {
            m.insert(key.into(), PathBuf::from(v));
        }
    }
    m
}

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_write_per_mtok: f64,
}

#[derive(Debug, Clone)]
pub struct PricingTable {
    pub models: HashMap<String, ModelPricing>,
    pub default: ModelPricing,
}

impl Default for PricingTable {
    fn default() -> Self {
        let mut models = HashMap::new();
        models.insert(
            "claude-opus-4".into(),
            ModelPricing {
                input_per_mtok: 15.0,
                output_per_mtok: 75.0,
                cache_read_per_mtok: 1.5,
                cache_write_per_mtok: 18.75,
            },
        );
        models.insert(
            "claude-sonnet-4".into(),
            ModelPricing {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cache_read_per_mtok: 0.3,
                cache_write_per_mtok: 3.75,
            },
        );
        models.insert(
            "claude-haiku".into(),
            ModelPricing {
                input_per_mtok: 0.8,
                output_per_mtok: 4.0,
                cache_read_per_mtok: 0.08,
                cache_write_per_mtok: 1.0,
            },
        );
        models.insert(
            "codex".into(),
            ModelPricing {
                input_per_mtok: 1.25,
                output_per_mtok: 10.0,
                cache_read_per_mtok: 0.125,
                cache_write_per_mtok: 0.0,
            },
        );
        models.insert(
            "gemini".into(),
            ModelPricing {
                input_per_mtok: 1.25,
                output_per_mtok: 10.0,
                cache_read_per_mtok: 0.31,
                cache_write_per_mtok: 0.0,
            },
        );
        models.insert(
            "kimi".into(),
            ModelPricing {
                input_per_mtok: 0.55,
                output_per_mtok: 2.20,
                cache_read_per_mtok: 0.15,
                cache_write_per_mtok: 0.0,
            },
        );
        Self {
            models,
            default: ModelPricing {
                input_per_mtok: 3.0,
                output_per_mtok: 15.0,
                cache_read_per_mtok: 0.3,
                cache_write_per_mtok: 3.75,
            },
        }
    }
}

impl PricingTable {
    /// Real session `model` fields are dated identifiers (e.g. "claude-opus-4-20250514"),
    /// so fall back to a prefix match against the family key when there's no exact hit.
    fn lookup(&self, model: &str) -> Option<&ModelPricing> {
        self.models.get(model).or_else(|| {
            self.models
                .iter()
                .find(|(key, _)| model.starts_with(key.as_str()))
                .map(|(_, p)| p)
        })
    }

    pub fn cost_for(
        &self,
        model: Option<&str>,
        tokens: &crate::model::TokenUsage,
    ) -> crate::model::Cost {
        let p = model.and_then(|m| self.lookup(m)).unwrap_or(&self.default);
        let input_usd = tokens.input as f64 / 1_000_000.0 * p.input_per_mtok;
        let output_usd = tokens.output as f64 / 1_000_000.0 * p.output_per_mtok;
        let cache_read_usd = tokens.cache_read as f64 / 1_000_000.0 * p.cache_read_per_mtok;
        let cache_write_usd = tokens.cache_write as f64 / 1_000_000.0 * p.cache_write_per_mtok;
        crate::model::Cost {
            total_usd: input_usd + output_usd + cache_read_usd + cache_write_usd,
            input_usd,
            output_usd,
            cache_read_usd,
            cache_write_usd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: Option<&str>) -> Self {
            let prev = std::env::var(key).ok();
            unsafe {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn resolve_home_prefers_userprofile_when_home_unset() {
        let _lock = env_lock().lock().unwrap();
        let _home = EnvGuard::set("HOME", None);
        let _up = EnvGuard::set("USERPROFILE", Some(r"C:\Users\fixture"));
        assert_eq!(resolve_home(), PathBuf::from(r"C:\Users\fixture"));
    }

    #[test]
    fn default_opencode_source_uses_userprofile_local_share() {
        let _lock = env_lock().lock().unwrap();
        let _home = EnvGuard::set("HOME", None);
        let _xdg = EnvGuard::set("XDG_DATA_HOME", None);
        let _oc_data = EnvGuard::set("OPENCODE_DATA_DIR", None);
        let _oc_dir = EnvGuard::set("OPENCODE_DIR", None);
        let _up = EnvGuard::set("USERPROFILE", Some(r"C:\Users\fixture"));
        let sources = default_sources();
        assert_eq!(
            sources.get("opencode").map(PathBuf::as_path),
            Some(Path::new(
                r"C:\Users\fixture\.local\share\opencode\opencode.db"
            ))
        );
    }
}
