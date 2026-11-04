//! Review attestation keyed by the staged tree hash.
//!
//! `agentflare git review --agent-reviewed|--vouch|--skip` writes
//! `<git-common-dir>/agentflare/attestations/<write-tree>.json`; the
//! `prepare-commit-msg` hook reads it back and stamps an
//! `Agentflare-Review: <status> (iter:N)` trailer (see `provenance`).
//! Keying by tree hash binds the claim to exactly the content being
//! committed: restaging yields a different tree and so no trailer.
//!
//! Same trust level as the other trailers: self-reported, NOT tamper-proof.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::shell::run_in_opt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewStatus {
    AgentReviewed,
    Vouched,
    Skipped,
}

impl ReviewStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentReviewed => "agent-reviewed",
            Self::Vouched => "vouched",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Attestation {
    status: ReviewStatus,
    iter: u32,
    ts: u64,
    agent: Option<String>,
}

fn dir(repo_root: &Path) -> Option<PathBuf> {
    let common = run_in_opt(
        repo_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Some(
        PathBuf::from(common)
            .join("agentflare")
            .join("attestations"),
    )
}

fn tree_hash(repo_root: &Path) -> Option<String> {
    run_in_opt(repo_root, &["write-tree"]).filter(|h| !h.is_empty())
}

fn counter_path(dir: &Path, branch: Option<&str>) -> PathBuf {
    let b: String = branch
        .unwrap_or("detached")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    dir.join(format!("iter-{b}"))
}

/// Records `status` for the currently staged tree; returns the iteration.
///
/// # Errors
/// If the repo, index tree, or attestation directory can't be resolved/written.
pub fn record(
    repo_root: &Path,
    status: ReviewStatus,
    agent: Option<String>,
    branch: Option<&str>,
) -> Result<u32, String> {
    let dir = dir(repo_root).ok_or("not a git repository")?;
    let tree = tree_hash(repo_root).ok_or("could not compute staged tree")?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let counter = counter_path(&dir, branch);
    let iter = fs::read_to_string(&counter)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
        + 1;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let body = serde_json::to_string(&Attestation {
        status,
        iter,
        ts,
        agent,
    })
    .map_err(|e| e.to_string())?;
    fs::write(&counter, iter.to_string()).map_err(|e| e.to_string())?;
    fs::write(dir.join(format!("{tree}.json")), body).map_err(|e| e.to_string())?;
    Ok(iter)
}

/// Trailer value (`agent-reviewed (iter:3)`) for the staged tree, or `None`
/// when no attestation matches it (fail-safe: never a false claim).
#[must_use]
pub fn current_review(repo_root: &Path) -> Option<String> {
    let path = dir(repo_root)?.join(format!("{}.json", tree_hash(repo_root)?));
    let a: Attestation = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    Some(format!("{} (iter:{})", a.status.as_str(), a.iter))
}

/// Resets the per-branch iteration counter after a commit was stamped.
pub fn reset_iter(repo_root: &Path, branch: Option<&str>) {
    if let Some(dir) = dir(repo_root) {
        let _ = fs::remove_file(counter_path(&dir, branch));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(p: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .current_dir(p)
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        git(d.path(), &["init", "-q"]);
        fs::write(d.path().join("a.txt"), "one").unwrap();
        git(d.path(), &["add", "a.txt"]);
        d
    }

    #[test]
    fn each_status_round_trips_to_a_trailer_value() {
        for (s, want) in [
            (ReviewStatus::AgentReviewed, "agent-reviewed (iter:1)"),
            (ReviewStatus::Vouched, "vouched (iter:1)"),
            (ReviewStatus::Skipped, "skipped (iter:1)"),
        ] {
            let d = repo();
            record(d.path(), s, None, Some("b")).unwrap();
            assert_eq!(current_review(d.path()).as_deref(), Some(want));
        }
    }

    #[test]
    fn no_attestation_means_no_review() {
        let d = repo();
        assert_eq!(current_review(d.path()), None);
    }

    #[test]
    fn restaging_invalidates_the_attestation() {
        let d = repo();
        record(d.path(), ReviewStatus::Vouched, None, None).unwrap();
        fs::write(d.path().join("a.txt"), "two").unwrap();
        git(d.path(), &["add", "a.txt"]);
        assert_eq!(current_review(d.path()), None);
    }

    #[test]
    fn iter_increments_then_resets() {
        let d = repo();
        assert_eq!(
            record(d.path(), ReviewStatus::Vouched, None, Some("b")),
            Ok(1)
        );
        assert_eq!(
            record(d.path(), ReviewStatus::Vouched, None, Some("b")),
            Ok(2)
        );
        reset_iter(d.path(), Some("b"));
        assert_eq!(
            record(d.path(), ReviewStatus::Vouched, None, Some("b")),
            Ok(1)
        );
    }
}
