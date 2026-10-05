//! `agentflare clean` engine: builds a cleanup [`Plan`] and applies selected
//! items. No terminal or network I/O; callers pass in what needs either.

mod apply;
pub mod artifacts;
mod merged;
mod worktrees;

pub use apply::{Outcome, Report, RestoreEntry, apply, purge};

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

/// `--only` / `--exclude`: a glob may name the label (a path, for worktrees
/// and artifacts) or the branch an item carries.
fn wanted(opts: &CleanOptions, label: &str, branch: Option<&str>) -> bool {
    let hit = |g: &String| glob_match(g, label) || branch.is_some_and(|b| glob_match(g, b));
    (opts.only.is_empty() || opts.only.iter().any(hit)) && !opts.exclude.iter().any(hit)
}

fn scan_git(input: &ScanInput, repo_root: &Path, plan: &mut Plan) {
    let opts = input.opts;
    // Never guessed from HEAD: a wrong "default" would make the real trunk
    // look merged into a feature branch.
    let Some(default) = crate::branch::resolve_default_branch_known(repo_root) else {
        plan.notes.push(
            "could not determine the default branch (no origin/HEAD, main or master): \
             branch and worktree cleanup skipped"
                .into(),
        );
        return;
    };
    if opts.branches || opts.worktrees {
        let branches = merged::classify_local(input, repo_root, &default);
        let (items, skipped) = worktrees::candidates(input, repo_root, branches);
        plan.items.extend(items);
        plan.skipped.extend(skipped);
    }
    if opts.remote {
        let (items, skipped) = merged::classify_remote(input, repo_root, &default);
        plan.items.extend(items);
        plan.skipped.extend(skipped);
    }
    if !input.pr_lookup_available {
        plan.notes.push(
            "GitHub lookup unavailable: squash-merged branches could not be verified and \
             are listed as not merged"
                .into(),
        );
    }
}

/// Builds the plan. Read-only: nothing is deleted, moved or fetched.
#[must_use]
pub fn scan(input: &ScanInput) -> Plan {
    let mut plan = Plan::default();
    let opts = input.opts;
    if opts.branches || opts.worktrees || opts.remote {
        match input.repo_root {
            Some(repo_root) => scan_git(input, repo_root, &mut plan),
            None => plan
                .notes
                .push("not a git repository: branch and worktree cleanup skipped".into()),
        }
    }
    if let Some(kinds) = &opts.artifacts {
        let (items, skipped) = artifacts::scan(input, kinds);
        plan.items.extend(items);
        plan.skipped.extend(skipped);
    }
    plan.items
        .retain(|i| wanted(opts, &i.label, i.branch.as_deref()));
    plan.skipped.retain(|s| wanted(opts, &s.label, None));
    // A worktree that is going away takes its artifacts with it.
    let doomed: Vec<PathBuf> = plan
        .items
        .iter()
        .filter(|i| matches!(i.kind, Kind::Worktree | Kind::Orphan))
        .filter_map(|i| i.path.clone())
        .collect();
    plan.items.retain(|i| {
        i.kind != Kind::Artifact
            || !i
                .path
                .as_ref()
                .is_some_and(|p| doomed.iter().any(|d| p.starts_with(d)))
    });
    // ...and one that is kept (open PR, unmerged, live process) keeps its
    // artifacts too: wiping an active checkout's build cache is a rebuild.
    let mut held = Vec::new();
    for i in std::mem::take(&mut plan.items) {
        let owner = (i.kind == Kind::Artifact)
            .then(|| {
                plan.skipped.iter().find(|s| {
                    i.label
                        .strip_prefix(&s.label)
                        .is_some_and(|r| r.starts_with('/'))
                })
            })
            .flatten();
        match owner {
            Some(s) => held.push(Skipped {
                label: i.label.clone(),
                reason: format!("inside skipped {}: {}", s.label, s.reason),
            }),
            None => plan.items.push(i),
        }
    }
    plan.skipped.extend(held);
    plan.items
        .sort_by(|a, b| (a.kind, b.size_bytes, &a.label).cmp(&(b.kind, a.size_bytes, &b.label)));
    plan.skipped.sort_by(|a, b| a.label.cmp(&b.label));
    plan
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
    use crate::shell::test_support::init_repo_with_branch;
    use crate::shell::{run_in, run_in_ok};

    pub(super) fn git_opts() -> CleanOptions {
        CleanOptions {
            branches: true,
            worktrees: true,
            ..Default::default()
        }
    }

    pub(super) fn scan_repo(repo: &Path, opts: &CleanOptions) -> Plan {
        let (claimed, states) = (HashSet::new(), HashMap::new());
        scan(&ScanInput {
            repo_root: Some(repo),
            scan_root: repo,
            opts,
            prs: &NoPrLookup,
            pr_lookup_available: false,
            claimed_items: &claimed,
            item_states: &states,
            live: &[],
        })
    }

    #[test]
    fn scan_is_read_only_and_notes_missing_pr_lookup() {
        let repo = init_repo_with_branch("master");
        run_in(&repo.path, &["branch", "done"]).unwrap();
        let plan = scan_repo(&repo.path, &git_opts());
        let ids: Vec<&str> = plan.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["branch:done"]);
        assert!(
            plan.notes.iter().any(|n| n.contains("squash")),
            "{:?}",
            plan.notes
        );
        assert!(
            run_in_ok(&repo.path, &["rev-parse", "--verify", "refs/heads/done"]),
            "scan deletes nothing"
        );
    }

    #[test]
    fn only_and_exclude_filter_by_label() {
        let repo = init_repo_with_branch("master");
        for b in ["task/1-a", "task/2-b", "keep"] {
            run_in(&repo.path, &["branch", b]).unwrap();
        }
        let labels = |o: CleanOptions| -> Vec<String> {
            let plan = scan_repo(&repo.path, &o);
            plan.items.into_iter().map(|i| i.label).collect()
        };
        let only = CleanOptions {
            only: vec!["task/*".into()],
            ..git_opts()
        };
        assert_eq!(labels(only), ["task/1-a", "task/2-b"]);
        let exclude = CleanOptions {
            exclude: vec!["task/1*".into()],
            ..git_opts()
        };
        assert_eq!(labels(exclude), ["keep", "task/2-b"]);
    }

    #[test]
    fn outside_a_repo_git_cleanup_is_noted_not_fatal() {
        let dir = tempfile::TempDir::new().unwrap();
        let (claimed, states) = (HashSet::new(), HashMap::new());
        let opts = git_opts();
        let plan = scan(&ScanInput {
            repo_root: None,
            scan_root: dir.path(),
            opts: &opts,
            prs: &NoPrLookup,
            pr_lookup_available: true,
            claimed_items: &claimed,
            item_states: &states,
            live: &[],
        });
        assert!(plan.items.is_empty());
        assert!(
            plan.notes
                .iter()
                .any(|n| n.contains("not a git repository"))
        );
    }

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

    fn add_worktree(repo: &Path, rel: &str, branch: &str) -> PathBuf {
        let path = repo.join(rel);
        let p = path.to_str().unwrap();
        run_in(repo, &["worktree", "add", "-b", branch, p]).unwrap();
        path
    }

    // Review finding 4: filters name a branch, and a worktree's label is its path.
    #[test]
    fn only_and_exclude_also_match_a_worktrees_branch() {
        let repo = init_repo_with_branch("master");
        add_worktree(&repo.path, ".worktrees/task/7", "task/7-x");
        let exclude = CleanOptions {
            exclude: vec!["task/7-*".into()],
            ..git_opts()
        };
        assert!(scan_repo(&repo.path, &exclude).items.is_empty());
        let only = CleanOptions {
            only: vec!["task/7-*".into()],
            ..git_opts()
        };
        assert_eq!(scan_repo(&repo.path, &only).items.len(), 1);
    }

    // Review finding 5: never guess the default branch from HEAD.
    #[test]
    fn unknown_default_branch_skips_git_cleanup_with_a_note() {
        let repo = init_repo_with_branch("develop");
        run_in(&repo.path, &["switch", "-c", "feat"]).unwrap();
        let plan = scan_repo(&repo.path, &git_opts());
        assert!(plan.items.is_empty(), "{:?}", plan.items);
        assert!(
            plan.notes.iter().any(|n| n.contains("default branch")),
            "{:?}",
            plan.notes
        );
    }

    // Review finding 9: a removable worktree already accounts for the
    // artifacts inside it.
    #[test]
    fn artifacts_inside_a_planned_worktree_are_not_planned_twice() {
        let repo = init_repo_with_branch("master");
        let r = &repo.path;
        std::fs::write(r.join(".gitignore"), "target/\n.worktrees/\n").unwrap();
        std::fs::write(r.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        run_in(r, &["add", "."]).unwrap();
        run_in(r, &["commit", "-m", "base"]).unwrap();
        let wt = add_worktree(r, ".worktrees/task/7", "task/7-x");
        std::fs::create_dir_all(wt.join("target")).unwrap();
        std::fs::write(wt.join("target/a.o"), b"obj").unwrap();
        let opts = CleanOptions {
            artifacts: Some(Vec::new()),
            ..git_opts()
        };
        let plan = scan_repo(r, &opts);
        let ids: Vec<&str> = plan.items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["worktree:.worktrees/task/7"]);
    }

    // An unmerged (kept) worktree keeps its build cache too.
    #[test]
    fn artifacts_inside_a_skipped_worktree_are_skipped_too() {
        let repo = init_repo_with_branch("master");
        let r = &repo.path;
        std::fs::write(r.join(".gitignore"), "target/\n.worktrees/\n").unwrap();
        std::fs::write(r.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        run_in(r, &["add", "."]).unwrap();
        run_in(r, &["commit", "-m", "base"]).unwrap();
        let wt = add_worktree(r, ".worktrees/task/8", "task/8-x");
        std::fs::write(wt.join("f.txt"), b"wip").unwrap();
        run_in(&wt, &["add", "."]).unwrap();
        run_in(&wt, &["commit", "-m", "unmerged"]).unwrap();
        std::fs::create_dir_all(wt.join("target")).unwrap();
        std::fs::write(wt.join("target/a.o"), b"obj").unwrap();
        let opts = CleanOptions {
            artifacts: Some(Vec::new()),
            ..git_opts()
        };
        let plan = scan_repo(r, &opts);
        assert!(plan.items.is_empty(), "{:?}", plan.items);
        let held = plan
            .skipped
            .iter()
            .find(|s| s.label == ".worktrees/task/8/target")
            .expect("target skipped");
        assert!(held.reason.starts_with("inside skipped .worktrees/task/8"));
    }
}
