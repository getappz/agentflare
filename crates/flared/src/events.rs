//! Append-only JSONL ledger of everything flared observed and did.

use std::path::PathBuf;

const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;
const TAIL_READ_BYTES: u64 = 256 * 1024;

pub struct EventLog {
    path: PathBuf,
    max_bytes: u64,
}

impl EventLog {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { path: dir.into().join("events.jsonl"), max_bytes: DEFAULT_MAX_BYTES }
    }

    #[cfg(test)]
    fn with_max_bytes(dir: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self { path: dir.into().join("events.jsonl"), max_bytes }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Append one event line: `{"ts": <unix>, "kind": ..., "detail": ...}`.
    /// When the ledger exceeds `max_bytes` it rotates to `events.jsonl.1`
    /// (single generation) so an always-on daemon never grows it unbounded.
    pub fn append(&self, kind: &str, detail: serde_json::Value) -> eyre::Result<()> {
        use std::io::Write;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if std::fs::metadata(&self.path).map(|m| m.len() >= self.max_bytes).unwrap_or(false) {
            let rotated = self.path.with_extension("jsonl.1");
            let _ = std::fs::remove_file(&rotated);
            let _ = std::fs::rename(&self.path, &rotated);
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let line = serde_json::json!({ "ts": ts, "kind": kind, "detail": detail });
        let mut file =
            std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Last `n` events, oldest first. Missing file -> empty. Reads only a
    /// bounded window from the end of the file, never the whole ledger.
    pub fn tail(&self, n: usize) -> eyre::Result<Vec<serde_json::Value>> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        let len = file.metadata()?.len();
        let start = len.saturating_sub(TAIL_READ_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        // A mid-file seek may land inside a line; drop the partial first line.
        let text = if start > 0 {
            text.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
        } else {
            text.as_str()
        };
        let events: Vec<serde_json::Value> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let skip = events.len().saturating_sub(n);
        Ok(events.into_iter().skip(skip).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn append_then_tail_returns_last_n_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::new(dir.path());
        for i in 0..3 {
            log.append("sweep", serde_json::json!({ "i": i })).unwrap();
        }
        let tail = log.tail(2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0]["detail"]["i"], 1);
        assert_eq!(tail[1]["detail"]["i"], 2);
        assert_eq!(tail[1]["kind"], "sweep");
        assert!(tail[1]["ts"].is_u64());
    }

    #[test]
    fn tail_on_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::new(dir.path());
        assert_eq!(log.tail(10).unwrap(), Vec::<serde_json::Value>::new());
    }

    #[test]
    fn ledger_rotates_when_over_max_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::with_max_bytes(dir.path(), 200);
        for i in 0..20 {
            log.append("sweep", serde_json::json!({ "i": i })).unwrap();
        }
        assert!(dir.path().join("events.jsonl.1").exists(), "rotation file missing");
        let main_len = std::fs::metadata(dir.path().join("events.jsonl")).unwrap().len();
        assert!(main_len < 400, "main ledger should have been rotated, is {main_len}");
        // Tail still returns the most recent event.
        let tail = log.tail(1).unwrap();
        assert_eq!(tail[0]["detail"]["i"], 19);
    }
}
