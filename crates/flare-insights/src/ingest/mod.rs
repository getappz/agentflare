pub mod claude;
pub mod codex;
pub mod common;
pub mod opencode;
pub mod watcher;

use std::path::Path;

use crate::config::InsightsConfig;
use crate::model::{Session, ToolCall, Turn};

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

#[derive(Debug, Default, Clone)]
pub struct IngestBundle {
    pub sessions: Vec<Session>,
    pub turns: Vec<Turn>,
    pub tool_calls: Vec<ToolCall>,
}

pub trait Adapter: Send + Sync {
    fn source_name(&self) -> &'static str;
    fn scan(&self, config: &InsightsConfig) -> Result<IngestBundle, IngestError>;
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

    /// DRY: scan all adapters, fail-open per source
    pub fn scan_all(&self, config: &InsightsConfig) -> Vec<(String, Result<IngestBundle, IngestError>)> {
        let mut out = Vec::new();
        for a in &self.adapters {
            let name = a.source_name().to_string();
            let res = a.scan(config);
            out.push((name, res));
        }
        out
    }

    pub fn scan_all_flat(&self, config: &InsightsConfig) -> IngestBundle {
        let mut bundle = IngestBundle::default();
        for (_, res) in self.scan_all(config) {
            if let Ok(b) = res {
                bundle.sessions.extend(b.sessions);
                bundle.turns.extend(b.turns);
                bundle.tool_calls.extend(b.tool_calls);
            }
        }
        bundle
    }
}

pub(crate) fn read_jsonl_sessions(path: &Path) -> Vec<serde_json::Value> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return vec![];
    };
    content
        .lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect()
}
