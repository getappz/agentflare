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

/// `git write-tree` against the index git is committing. `shell::run_in`
/// scrubs `GIT_INDEX_FILE`, but under `git commit -a`/`<paths>` the hook
/// runs with a temporary index in it, so carry that one var through.
fn tree_hash(repo_root: &Path) -> Option<String> {
    let mut cmd = std::process::Command::new(crate::shell::git_binary());
    cmd.arg("write-tree").current_dir(repo_root);
    crate::shell::apply_filtered_path(&mut cmd);
    if let Some(idx) = std::env::var_os("GIT_INDEX_FILE") {
        cmd.env("GIT_INDEX_FILE", idx);
    }
    let out = cmd.output().ok().filter(|o| o.status.success())?;
    let h = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!h.is_empty()).then_some(h)
}

fn counter_path(dir: &Path, branch: Option<&str>) -> PathBuf {
    let b: String = branch
        .unwrap_or("detached")
        .bytes()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                (c as char).to_string()
            } else {
                format!("_{c:02x}")
            }
        })
        .collect();
    dir.join(format!("iter-{b}"))
}

/// Write via tmp + rename so a crash never leaves a half-written file.
fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, body).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Drops attestations untouched for a week (tree hashes are single-use).
fn prune(dir: &Path) {
    const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > MAX_AGE);
        if old {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Records `status` for the currently staged tree; returns the iteration.
///
/// The counter file holds `<HEAD sha> <n>`: it only counts while HEAD is
/// unchanged, so a new commit restarts it at 1 with no post-commit hook and
/// an aborted commit loses nothing.
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
    prune(&dir);
    let head = run_in_opt(repo_root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "none".into());
    let counter = counter_path(&dir, branch);
    let iter = fs::read_to_string(&counter)
        .ok()
        .and_then(|s| {
            let (h, n) = s.trim().split_once(' ')?;
            (h == head).then(|| n.parse::<u32>().ok())?
        })
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
    // Attestation first: if it fails the counter must not advance.
    write_atomic(&dir.join(format!("{tree}.json")), &body)?;
    write_atomic(&counter, &format!("{head} {iter}"))?;
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
    fn iter_increments_then_restarts_after_a_commit() {
        let d = repo();
        let rec = || record(d.path(), ReviewStatus::Vouched, None, Some("b"));
        assert_eq!(rec(), Ok(1));
        assert_eq!(rec(), Ok(2));
        git(
            d.path(),
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "x",
            ],
        );
        fs::write(d.path().join("a.txt"), "two").unwrap();
        git(d.path(), &["add", "a.txt"]);
        assert_eq!(rec(), Ok(1));
    }

    #[test]
    fn branch_names_do_not_collide() {
        let d = std::path::Path::new("/x");
        assert_ne!(
            counter_path(d, Some("task/1")),
            counter_path(d, Some("task_1"))
        );
    }
}
