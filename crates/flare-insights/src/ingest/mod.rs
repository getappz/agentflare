pub mod claude;
pub mod codex;
pub mod common;
pub mod opencode;
pub mod watcher;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::InsightsConfig;
use crate::model::{FileEvent, Session, Subagent, ToolCall, Turn};
use crate::store::InsightsStore;

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

/// Per-source `path -> (mtime_ms, size)` cache from the last successful sync,
/// used by file-walking adapters to skip re-parsing unchanged session files.
pub type FileCursors = HashMap<PathBuf, (i64, u64)>;

#[derive(Debug, Default, Clone)]
pub struct IngestBundle {
    pub sessions: Vec<Session>,
    pub turns: Vec<Turn>,
    pub tool_calls: Vec<ToolCall>,
    pub file_events: Vec<FileEvent>,
    pub subagents: Vec<Subagent>,
    /// Updated cursor entries for files this scan actually parsed (not for
    /// files skipped because they were already up to date).
    pub file_cursors: Vec<(PathBuf, i64, u64)>,
}

pub trait Adapter: Send + Sync {
    fn source_name(&self) -> &'static str;
    fn scan(
        &self,
        config: &InsightsConfig,
        cursors: &FileCursors,
        on_progress: &mut dyn FnMut(usize, usize),
    ) -> Result<IngestBundle, IngestError>;
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

    /// DRY: scan all adapters, fail-open per source. `on_progress` is called
    /// as `(source, files_done, files_total)` while each adapter walks its
    /// files, so callers can render live sync progress.
    pub fn scan_all(
        &self,
        config: &InsightsConfig,
        store: &InsightsStore,
        mut on_progress: impl FnMut(&str, usize, usize),
    ) -> Vec<(String, Result<IngestBundle, IngestError>)> {
        let mut out = Vec::new();
        for a in &self.adapters {
            let name = a.source_name().to_string();
            let cursors = store.load_file_cursors(&name).unwrap_or_default();
            let res = a.scan(config, &cursors, &mut |done, total| {
                on_progress(&name, done, total)
            });
            out.push((name, res));
        }
        out
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
