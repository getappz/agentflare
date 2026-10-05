//! Opt-in local training-data capture (`AGENTFLARE_DECIDE_CAPTURE=1`): one row
//! per Jev answer with the (redacted, truncated) input features and Jev's
//! label, in `~/.agentflare/decide/dataset.jsonl`, so a deterministic engine
//! can later be distilled from it. Unlike the shadow log this DOES contain
//! user prompt text; it stays on this machine, is size-capped, and is removed
//! with `agentflare decide dataset clear`. Writing is best-effort.
mod features;
mod norm;
mod stats;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

pub use features::{judge_features, rerank_features, router_features};
pub use norm::{norm_key, normalize};
pub use stats::{render, summarize};

const MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub schema: u32,
    pub ts: i64,
    pub site: String,
    pub features: Value,
    pub norm_key: String,
    /// Jev's answer; `summary` is the short label used for balance stats.
    pub label: Value,
    #[serde(default)]
    pub confidence: Option<f64>,
    pub baseline: String,
    #[serde(default)]
    pub source_model: Option<String>,
}

/// What a decision site hands over after Jev answered.
pub struct Input<'a> {
    pub site: &'a str,
    pub features: Value,
    /// Text whose normalized hash is the row's `norm_key`.
    pub norm_input: &'a str,
    pub label: Value,
    pub confidence: Option<f64>,
    pub baseline: &'a str,
    pub source_model: Option<&'a str>,
}

pub fn enabled() -> bool {
    std::env::var("AGENTFLARE_DECIDE_CAPTURE").as_deref() == Ok("1")
}

pub fn dataset_path() -> PathBuf {
    crate::paths::agentflare_dir()
        .join("decide")
        .join("dataset.jsonl")
}

fn rotated(path: &Path) -> PathBuf {
    path.with_extension("jsonl.1")
}

pub fn build_row(i: Input<'_>) -> Row {
    Row {
        schema: SCHEMA,
        ts: chrono::Utc::now().timestamp(),
        site: i.site.to_string(),
        features: norm::sanitize_features(i.features),
        norm_key: norm::norm_key(&crate::mcp_server::secret_scan::redact(i.norm_input)),
        label: i.label,
        confidence: i.confidence,
        baseline: i.baseline.to_string(),
        source_model: i.source_model.map(str::to_string),
    }
}

/// No-op unless opted in; errors are deliberately ignored.
pub fn record(i: Input<'_>) {
    if enabled() {
        let _ = append(&dataset_path(), &build_row(i), MAX_BYTES);
    }
}

pub(crate) fn append(path: &Path, row: &Row, max_bytes: u64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > max_bytes) {
        let old = rotated(path);
        let _ = std::fs::remove_file(&old); // rename won't overwrite on Windows
        std::fs::rename(path, old)?;
    }
    let mut line = serde_json::to_string(row).map_err(std::io::Error::other)?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

/// Rotated file first (older rows), then the live one; damaged lines skipped.
pub fn load(path: &Path) -> Vec<Row> {
    [rotated(path), path.to_path_buf()]
        .iter()
        .flat_map(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect::<Vec<Row>>()
        })
        .collect()
}

/// Delete the dataset (and its rotated half); returns how many files went.
pub fn clear(path: &Path) -> usize {
    [path.to_path_buf(), rotated(path)]
        .iter()
        .filter(|p| std::fs::remove_file(p).is_ok())
        .count()
}

/// Jev's answer as a label: `summary` plus any probabilities.
pub fn label_of(answer: &crate::decide::Answer) -> Value {
    use crate::decide::Answer;
    match answer {
        Answer::Choice {
            choice,
            probabilities,
            ..
        } => serde_json::json!({ "summary": choice, "probabilities": probabilities }),
        Answer::Score {
            score,
            probabilities,
            ..
        } => serde_json::json!({
            "summary": format!("{score:.1}"),
            "score": score,
            "probabilities": probabilities,
        }),
        Answer::Noul { noul } => serde_json::json!({ "summary": "noul", "noul": noul }),
    }
}
