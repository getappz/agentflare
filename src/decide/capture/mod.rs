//! Opt-in local training-data capture (`AGENTFLARE_DECIDE_CAPTURE=1`): one row
//! per Jev answer with the (redacted, truncated) input features and Jev's
//! label, in `~/.agentflare/decide/dataset.jsonl`, so a deterministic engine
//! can later be distilled from it. Unlike the shadow log this DOES contain
//! user prompt text; it stays on this machine, is size-capped, and is removed
//! with `agentflare decide dataset clear`. Writing is best-effort.
mod features;
pub(crate) mod norm;
mod stats;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub use features::{judge_features, rerank_features, router_features};
pub use norm::{norm_key, normalize};
pub use stats::{render, summarize};

const MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    #[serde(default)]
    pub decision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Value>,
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
        decision_id: None,
        question: None,
        answer: None,
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
        super::storage::capture(build_row(i));
    }
}

pub(crate) fn append(path: &Path, row: &impl Serialize, max_bytes: u64) -> std::io::Result<()> {
    append_many(
        path,
        std::iter::once((0, row)),
        max_bytes,
        |_| true,
        |_| {},
        false,
    )
}

pub(crate) fn append_many<'a, T: Serialize + 'a>(
    path: &Path,
    rows: impl IntoIterator<Item = (u64, &'a T)>,
    max_bytes: u64,
    mut allowed: impl FnMut(u64) -> bool,
    mut written: impl FnMut(u64),
    recover: bool,
) -> std::io::Result<()> {
    let mut lines = Vec::new();
    for (ticket, row) in rows {
        let mut line = serde_json::to_string(row).map_err(std::io::Error::other)?;
        line.push('\n');
        if line.len() > 64 * 1024 {
            return Err(std::io::Error::other("decision record exceeds 64 KiB"));
        }
        lines.push((ticket, line));
    }
    if lines.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(parent)?;
        restrict(parent, path)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).read(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut file = opts.open(path)?;
    // Never wait for another process; contention is counted as a dropped write.
    fs2::FileExt::try_lock_exclusive(&file)?;
    repair_tail(&mut file, max_bytes)?;
    #[cfg(test)]
    if std::env::var("AGENTFLARE_TEST_LEDGER_LOCK_DELAY").as_deref() == Ok("1") {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let mut seen = std::collections::BTreeSet::new();
    if recover {
        let mut bytes = match std::fs::read(rotated(path)) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        bytes.push(b'\n');
        file.seek(SeekFrom::Start(0))?;
        Read::by_ref(&mut file)
            .take(max_bytes)
            .read_to_end(&mut bytes)?;
        for line in String::from_utf8_lossy(&bytes).lines() {
            if let Ok(value) = serde_json::from_str::<Value>(line)
                && value.get("site").is_some()
                && let Some(id) = value.get("decision_id").and_then(Value::as_str)
            {
                seen.insert(id.to_owned());
            }
        }
    }
    let mut len = file.metadata()?.len();
    for (ticket, line) in lines {
        if !allowed(ticket) {
            continue;
        }
        let id = if recover {
            serde_json::from_str::<Value>(&line).ok().and_then(|v| {
                v.get("decision_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
        } else {
            None
        };
        if id.as_ref().is_some_and(|id| seen.contains(id)) {
            written(ticket);
            continue;
        }
        if len + line.len() as u64 > max_bytes {
            // Windows permits neither replacing nor renaming an open locked file.
            // Keep the lock inode stable: copy the bounded old bytes, then truncate.
            file.seek(SeekFrom::Start(0))?;
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(max_bytes)
                .read_to_end(&mut bytes)?;
            let mut backup = std::fs::OpenOptions::new();
            backup.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut backup, 0o600);
            backup.open(rotated(path))?.write_all(&bytes)?;
            file.set_len(0)?;
            len = 0;
        }
        file.seek(SeekFrom::End(0))?;
        if let Err(e) = file.write_all(line.as_bytes()) {
            let _ = file.set_len(len);
            return Err(e);
        }
        len += line.len() as u64;
        written(ticket);
        if let Some(id) = id {
            seen.insert(id);
        }
    }
    Ok(())
}

fn repair_tail(file: &mut std::fs::File, max_bytes: u64) -> std::io::Result<()> {
    let len = file.metadata()?.len();
    if len == 0 || len > max_bytes {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    Read::by_ref(file).take(max_bytes).read_to_end(&mut bytes)?;
    let complete = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    if serde_json::from_slice::<Value>(&bytes[complete..]).is_ok() {
        file.seek(SeekFrom::End(0))?;
        file.write_all(b"\n")?;
    } else {
        // Exit can interrupt write_all before its error rollback runs.
        file.set_len(complete as u64)?;
    }
    Ok(())
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
