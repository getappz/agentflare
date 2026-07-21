//! Append-only, generic audit logging: any `Serialize`/`Deserialize` event
//! type can be appended as one JSONL line and read back. Used for two
//! distinct logs -- the git-shim's own classified events
//! (`classify::Event`, `default_path("git.jsonl")`) and the
//! `reference-transaction` hook's backstop ref-move journal
//! (`RefTransactionEvent`, `default_path("git-refs.jsonl")`), which fires
//! for every ref move in a repo regardless of whether git was invoked
//! through the shim at all.

use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// One ref update from a `reference-transaction` hook invocation --
/// `<old-oid> <new-oid> <refname>`, one per line on the hook's stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefTransaction {
    pub old: String,
    pub new: String,
    pub refname: String,
}

/// A committed `reference-transaction`: the agent identity (if any --
/// self-reported, see `provenance::build_trailers`) plus every ref it moved
/// in one transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefTransactionEvent {
    pub agent: Option<String>,
    pub transactions: Vec<RefTransaction>,
}

/// Default location for a named audit log under `~/.agentflare/audit/`.
/// Honors `AGENTFLARE_HOME_OVERRIDE` (the main binary's own test/CI escape
/// hatch, see `src/paths.rs::home` -- `dirs::home_dir()` resolves via the
/// OS directly on Windows and ignores HOME/USERPROFILE overrides) so tests
/// never write into a developer's real home directory.
#[must_use]
pub fn default_path(name: &str) -> Option<PathBuf> {
    let home = match std::env::var("AGENTFLARE_HOME_OVERRIDE") {
        Ok(p) => PathBuf::from(p),
        Err(_) => dirs::home_dir()?,
    };
    Some(home.join(".agentflare").join("audit").join(name))
}

/// Byte size at which an audit log gets trimmed back down to
/// `AUDIT_LOG_KEEP_LINES` — an append-only JSONL log otherwise grows forever
/// (every git subcommand invocation fires one event). Checked via a single
/// `metadata()` stat on every append, so the common under-budget case costs
/// one cheap syscall, not a read of the whole file.
const AUDIT_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
const AUDIT_LOG_KEEP_LINES: usize = 5_000;

/// Appends one JSONL line for `event`, creating the parent directory and
/// file if needed.
pub fn log_event<T: Serialize>(audit_path: &Path, event: &T) -> std::io::Result<()> {
    if let Some(parent) = audit_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(audit_path)?;
    let line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    writeln!(f, "{line}")?;
    drop(f);
    maybe_rotate(audit_path, AUDIT_LOG_MAX_BYTES, AUDIT_LOG_KEEP_LINES)
}

/// Trims `path` down to its last `keep_lines` lines, oldest dropped first,
/// once it exceeds `max_bytes`. No-op (single stat, no read) while under
/// budget.
fn maybe_rotate(path: &Path, max_bytes: u64, keep_lines: usize) -> std::io::Result<()> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.len() <= max_bytes {
        return Ok(());
    }
    let content = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= keep_lines {
        return Ok(());
    }
    let trimmed = lines[lines.len() - keep_lines..].join("\n");
    std::fs::write(path, trimmed + "\n")
}

/// Reads back every event in the log, oldest first. A missing file reads
/// as empty (nothing has been logged yet — not an error). A malformed line
/// fails closed: returns an error rather than silently skipping it, since
/// a corrupt audit entry is a bug worth surfacing, not data to quietly drop.
pub fn read_events<T: for<'de> Deserialize<'de>>(audit_path: &Path) -> std::io::Result<Vec<T>> {
    let content = match std::fs::read_to_string(audit_path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(std::io::Error::other))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{Disposition, Event};
    use tempfile::TempDir;

    fn sample_event(subcommand: &str) -> Event {
        Event {
            subcommand: subcommand.to_string(),
            args: vec!["origin".to_string()],
            disposition: Disposition::Passthrough,
        }
    }

    #[test]
    fn reading_a_missing_log_is_empty_not_an_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist").join("git.jsonl");
        assert_eq!(read_events::<Event>(&path).unwrap(), Vec::new());
    }

    #[test]
    fn append_then_read_back_round_trips_in_order() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("audit").join("git.jsonl");
        log_event(&path, &sample_event("fetch")).unwrap();
        log_event(&path, &sample_event("push")).unwrap();
        let events: Vec<Event> = read_events(&path).unwrap();
        assert_eq!(events, vec![sample_event("fetch"), sample_event("push")]);
    }

    #[test]
    fn deny_events_round_trip_with_their_reason() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("git.jsonl");
        let event = Event {
            subcommand: "checkout".to_string(),
            args: vec!["master".to_string()],
            disposition: Disposition::Deny {
                reason: "protected branch".to_string(),
            },
        };
        log_event(&path, &event).unwrap();
        assert_eq!(read_events::<Event>(&path).unwrap(), vec![event]);
    }

    #[test]
    fn a_malformed_line_fails_closed_instead_of_being_silently_dropped() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("git.jsonl");
        log_event(&path, &sample_event("fetch")).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"not valid json\n")
            .unwrap();
        assert!(
            read_events::<Event>(&path).is_err(),
            "a corrupt line must surface as an error, not be silently skipped"
        );
    }

    #[test]
    fn maybe_rotate_is_a_noop_under_budget() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("git.jsonl");
        for i in 0..10 {
            log_event(&path, &sample_event(&format!("cmd{i}"))).unwrap();
        }
        assert_eq!(read_events::<Event>(&path).unwrap().len(), 10);
    }

    #[test]
    fn maybe_rotate_trims_oldest_lines_once_over_budget() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("git.jsonl");
        for i in 0..20 {
            log_event(&path, &sample_event(&format!("cmd{i}"))).unwrap();
        }
        // Tiny budget forces rotation on the very next append.
        maybe_rotate(&path, 1, 5).unwrap();
        let events = read_events::<Event>(&path).unwrap();
        assert_eq!(events.len(), 5, "only the 5 most recent survive");
        assert_eq!(events[0].subcommand, "cmd15", "oldest dropped first");
        assert_eq!(events[4].subcommand, "cmd19", "newest kept");
    }

    #[test]
    fn maybe_rotate_on_a_missing_file_is_a_noop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");
        assert!(maybe_rotate(&path, 1, 5).is_ok());
    }

    #[test]
    fn ref_transaction_events_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("git-refs.jsonl");
        let event = RefTransactionEvent {
            agent: Some("claude-code".to_string()),
            transactions: vec![RefTransaction {
                old: "0".repeat(40),
                new: "a".repeat(40),
                refname: "refs/heads/feature/x".to_string(),
            }],
        };
        log_event(&path, &event).unwrap();
        assert_eq!(
            read_events::<RefTransactionEvent>(&path).unwrap(),
            vec![event]
        );
    }
}
