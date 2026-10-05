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
    enabled_for(std::env::var("AGENTFLARE_DECIDE_CAPTURE").ok().as_deref())
}

fn enabled_for(var: Option<&str>) -> bool {
    var == Some("1")
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
        label: norm::redact_value(i.label),
        confidence: i.confidence,
        baseline: crate::mcp_server::secret_scan::redact(i.baseline),
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
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(parent)?;
        restrict(parent, path)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > max_bytes) {
        // rename replaces the target; a lost race or locked file must not drop the row.
        let _ = std::fs::rename(path, rotated(path));
    }
    let mut line = serde_json::to_string(row).map_err(std::io::Error::other)?;
    line.push('\n');
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)?.write_all(line.as_bytes())
}

/// Unix: dir 0o700, any pre-existing dataset/rotated file 0o600. Errors
/// propagate so nothing is appended into a dir we could not lock down.
#[cfg(unix)]
fn restrict(dir: &Path, path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    for p in [path.to_path_buf(), rotated(path)] {
        match std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            r => r?,
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_dir: &Path, _path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Rotated file first (older rows), then the live one; damaged lines skipped.
pub fn load(path: &Path) -> Vec<Row> {
    [rotated(path), path.to_path_buf()]
        .iter()
        .flat_map(|p| {
            let bytes = std::fs::read(p).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            text.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect::<Vec<Row>>()
        })
        .collect()
}

/// Delete the dataset (and its rotated half); returns how many files went.
/// A missing file counts as already clear; any other error is returned
/// (after attempting both files).
pub fn clear(path: &Path) -> std::io::Result<usize> {
    let mut removed = 0;
    let mut first_err = None;
    for p in [path.to_path_buf(), rotated(path)] {
        match std::fs::remove_file(&p) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    first_err.map_or(Ok(removed), Err)
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
