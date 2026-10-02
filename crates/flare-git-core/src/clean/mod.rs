//! `agentflare clean` engine: builds a cleanup [`Plan`] and applies selected
//! items. No terminal or network I/O; callers pass in what needs either.

pub mod artifacts;
mod merged;
mod worktrees;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

/// Order is the display order of the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Branch,
    Worktree,
    Orphan,
    Artifact,
    Remote,
}

/// One thing the plan proposes to remove.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Item {
    /// Stable across runs: `branch:<name>`, `worktree:<label>`,
    /// `orphan:<name>`, `artifact:<label>`, `remote:<name>`.
    pub id: String,
    pub kind: Kind,
    /// Branch name, or path relative to the scan root.
    pub label: String,
    /// Absolute path, for Worktree/Orphan/Artifact.
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    /// Tip at plan time; apply refuses if it moved.
    pub sha: Option<String>,
    pub size_bytes: u64,
    pub reason: String,
}

/// Something considered and deliberately left alone, with why.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Skipped {
    pub label: String,
    pub reason: String,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Plan {
    pub items: Vec<Item>,
    pub skipped: Vec<Skipped>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone)]
pub struct PrInfo {
    pub number: u64,
    pub state: PrState,
    pub head_sha: String,
}

/// Pull-request lookup by head branch. The engine does no network I/O, so
/// the caller supplies this (GitHub-backed in the CLI, a fake in tests).
pub trait PrLookup: Sync {
    fn for_branch(&self, branch: &str) -> Option<PrInfo>;
}

pub struct NoPrLookup;

impl PrLookup for NoPrLookup {
    fn for_branch(&self, _: &str) -> Option<PrInfo> {
        None
    }
}

#[derive(Debug, Clone, Default)]
pub struct CleanOptions {
    pub branches: bool,
    pub worktrees: bool,
    /// `None` = off, `Some(empty)` = every kind.
    pub artifacts: Option<Vec<String>>,
    pub remote: bool,
    pub older_than: Option<std::time::Duration>,
    pub min_size: u64,
    pub only: Vec<String>,
    pub exclude: Vec<String>,
}

pub struct ScanInput<'a> {
    /// `None` when the scan root is not inside a git repository.
    pub repo_root: Option<&'a Path>,
    pub scan_root: &'a Path,
    pub opts: &'a CleanOptions,
    pub prs: &'a dyn PrLookup,
    /// `false` adds a note that squash-merged branches could not be verified.
    pub pr_lookup_available: bool,
    /// Sequence ids (as strings) with a live claim.
    pub claimed_items: &'a HashSet<String>,
    /// Sequence id (as a string) -> state group.
    pub item_states: &'a HashMap<String, String>,
    pub live: &'a [flare_process::cwd::LiveProc],
}

/// `f` over `items` on a bounded worker pool, results in input order.
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
        .min(items.len());
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<Option<R>>> = Mutex::new(items.iter().map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = f(item);
                    out.lock().unwrap_or_else(PoisonError::into_inner)[i] = Some(r);
                }
            });
        }
    });
    out.into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .into_iter()
        .map(|r| r.expect("every slot is filled by exactly one worker"))
        .collect()
}

/// Wildcard match where `*` is any run of characters, `/` included.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_match_star_spans_slashes() {
        assert!(glob_match("task/30*", "task/300-human"));
        assert!(glob_match("*/node_modules", "apps/web/node_modules"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("task/30*", "task/310"));
        assert!(!glob_match("a*b", "a/c"));
    }

    #[test]
    fn par_map_keeps_order() {
        let v: Vec<u32> = (0..100).collect();
        assert_eq!(
            par_map(&v, |x| x * 2),
            v.iter().map(|x| x * 2).collect::<Vec<_>>()
        );
        assert!(par_map(&Vec::<u32>::new(), |x| *x).is_empty());
    }
}
