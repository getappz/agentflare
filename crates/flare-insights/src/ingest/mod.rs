pub mod claude;
pub mod codex;
pub mod opencode;
pub mod watcher;

use std::path::Path;

use crate::config::InsightsConfig;
use crate::model::Session;

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("unsupported source: {0}")]
    Unsupported(String),
}

pub trait Adapter: Send + Sync {
    fn source_name(&self) -> &'static str;
    fn scan(&self, config: &InsightsConfig) -> Result<Vec<Session>, IngestError>;
}

pub struct IngestManager {
    adapters: Vec<Box<dyn Adapter>>,
}

impl Default for IngestManager {
    fn default() -> Self {
        Self::new()
    }
}

impl IngestManager {
    pub fn new() -> Self {
        Self {
            adapters: vec![
                Box::new(claude::ClaudeAdapter),
                Box::new(codex::CodexAdapter),
                Box::new(opencode::OpenCodeAdapter),
            ],
        }
    }

    pub fn scan_all(&self, config: &InsightsConfig) -> Vec<(String, Result<Vec<Session>, IngestError>)> {
        let mut out = Vec::new();
        for a in &self.adapters {
            let name = a.source_name().to_string();
            // fail-open: one adapter failure never blocks others
            let res = a.scan(config);
            out.push((name, res));
        }
        out
    }

    pub fn scan_all_flat(&self, config: &InsightsConfig) -> Vec<Session> {
        let mut all = Vec::new();
        for (_, res) in self.scan_all(config) {
            if let Ok(sessions) = res {
                all.extend(sessions);
            }
        }
        all
    }
}

pub(crate) fn read_jsonl_sessions(path: &Path) -> Vec<serde_json::Value> {
    let Ok(content) = std::fs::read_to_string(path) else { return vec![]; };
    content.lines().filter_map(|l| serde_json::from_str(l.trim()).ok()).collect()
}
