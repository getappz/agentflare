//! Minimal append-only audit trail for `gateway_execute` calls: timestamp,
//! server, tool, a hash of the args (never the raw args — they may contain
//! secrets), and outcome. Same category of guard forgemax's
//! `forge-audit` documents; written fresh here, not ported — see
//! `error.rs`'s note on forgemax's FSL license.
//!
//! Logging failures (disk full, permissions) must never break the actual
//! tool call — every write here is best-effort, errors just go to stderr.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::Path;

fn hash_args(args: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(args).unwrap_or_default());
    format!("{:x}", hasher.finalize())
}

pub fn record(path: &Path, server: &str, tool: &str, args: &Value, outcome: Result<(), &str>) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = match outcome {
        Ok(()) => serde_json::json!({
            "ts": ts, "server": server, "tool": tool, "args_hash": hash_args(args), "outcome": "ok",
        }),
        Err(error_kind) => serde_json::json!({
            "ts": ts, "server": server, "tool": tool, "args_hash": hash_args(args),
            "outcome": "err", "error_kind": error_kind,
        }),
    };
    match std::fs::OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{line}") {
                eprintln!("gateway-registry: failed to write audit log entry to {}: {e}", path.display());
            }
        }
        Err(e) => eprintln!("gateway-registry: failed to open audit log {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_never_contains_the_raw_secret() {
        let args = serde_json::json!({"api_key": "sk-super-secret-value"});
        let hash = hash_args(&args);
        assert!(!hash.contains("sk-super-secret-value"));
        assert_eq!(hash.len(), 64, "sha256 hex digest should be 64 chars");
    }

    #[test]
    fn same_args_hash_identically() {
        let args = serde_json::json!({"query": "x", "limit": 5});
        assert_eq!(hash_args(&args), hash_args(&args));
    }

    #[test]
    fn record_appends_a_line_with_expected_fields_and_no_raw_args() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path();
        let args = serde_json::json!({"query": "do-not-leak-me", "limit": 5});

        record(path, "memory_store", "search", &args, Ok(()));
        record(path, "memory_store", "search", &args, Err("Upstream"));

        let contents = std::fs::read_to_string(path).unwrap();
        assert!(!contents.contains("do-not-leak-me"), "{contents}");
        let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2);

        let first: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["server"], "memory_store");
        assert_eq!(first["tool"], "search");
        assert_eq!(first["outcome"], "ok");
        assert!(first["args_hash"].is_string());

        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["outcome"], "err");
        assert_eq!(second["error_kind"], "Upstream");
    }
}
