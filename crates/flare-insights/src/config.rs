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

fn default_sources() -> HashMap<String, PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let h = PathBuf::from(home);
    let mut m = HashMap::new();
    m.insert("claude_code".into(), h.join(".claude").join("projects"));
    m.insert("codex".into(), h.join(".codex").join("sessions"));
    m.insert(
        "opencode".into(),
        h.join(".local")
            .join("share")
            .join("opencode")
            .join("opencode.db"),
    );
    m.insert("cursor".into(), h.join(".config").join("Cursor"));
    m.insert("gemini".into(), h.join(".gemini"));
    m.insert("copilot".into(), h.join(".copilot"));
    // env overrides
    for (key, env) in [
        ("claude_code", "CLAUDE_PROJECTS_DIR"),
        ("codex", "CODEX_SESSIONS_DIR"),
        ("opencode", "OPENCODE_DIR"),
        ("cursor", "CURSOR_DIR"),
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
