//! Thin wrapper around `flare_git_core::worktree` — the local git/worktree
//! mechanics live there now. This file only adds what that leaf crate
//! deliberately does NOT know about: the main binary's MCP-specific
//! `ProgressSender` (depends on `rmcp`), and opening a GitHub PR once a
//! branch is pushed (depends on `src/github`, a GitHub-REST concern kept
//! out of flare-git-core on purpose).

use std::path::{Path, PathBuf};

use crate::github::identity::RepoId;
use crate::progress::ProgressSender;

impl flare_git_core::worktree::Progress for ProgressSender {
    fn send(&self, progress: f64, total: Option<f64>, message: Option<String>) {
        ProgressSender::send(self, progress, total, message);
    }
}

fn as_progress(p: Option<&ProgressSender>) -> Option<&dyn flare_git_core::worktree::Progress> {
    p.map(|p| p as &dyn flare_git_core::worktree::Progress)
}

pub use flare_git_core::worktree::resolve_target_branch;

/// Whether `item`'s branch has any committed content `target_branch`
/// doesn't already have. See `flare_git_core::worktree::branch_diverged`.
pub fn branch_diverged(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    target_branch: &str,
) -> bool {
    let branch = flare_git_core::worktree::task_branch_name(item);
    flare_git_core::worktree::branch_diverged(repo_root, &branch, target_branch)
}

pub fn create_worktree(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    target_branch: &str,
    progress: Option<&ProgressSender>,
) -> Result<PathBuf, String> {
    flare_git_core::worktree::create_worktree(item, repo_root, target_branch, as_progress(progress))
}

/// The `done`-side counterpart to `create_worktree`: removes it now that the
/// item is finished, if its tree is clean. Best-effort like
/// `push_and_open_pr` — never blocks `done` on a cleanup failure.
pub fn cleanup_worktree(item: &agentflare_backend::item::Item, repo_root: &Path) {
    flare_git_core::worktree::cleanup_item_worktree(item, repo_root);
}

pub use flare_git_core::worktree::CommitOutcome;

/// Commits any uncommitted changes in `item`'s worktree. See
/// `flare_git_core::worktree::commit_uncommitted`.
pub fn commit_uncommitted(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    message: &str,
) -> CommitOutcome {
    flare_git_core::worktree::commit_uncommitted(item, repo_root, message)
}

/// Checks whether `item`'s branch already has a merged PR — the promotion
/// signal `check_merge` uses to move an item out of "in_review" (item
/// #420). Soft-fails like `push_and_open_pr`: no GitHub credentials, no
/// resolvable remote, or a lookup failure all just report "not merged yet"
/// rather than erroring, since the caller's fallback is simply to check
/// again later.
pub fn is_pr_merged(item: &agentflare_backend::item::Item, repo_root: &Path) -> bool {
    let branch = flare_git_core::worktree::task_branch_name(item);
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return false;
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(_) => return false,
    };
    match crate::github::pulls::find_existing(&client, &repo, &branch) {
        Ok(Some(pr)) => pr.merged_at.is_some(),
        Ok(None) => false,
        Err(e) => {
            eprintln!(
                "worktree: could not check merge status for item {}: {e}",
                item.id
            );
            false
        }
    }
}

/// Swaps a merged PR's stage label from `agentflare:in-review` to
/// `agentflare:completed` once `check_merge` has confirmed the merge and
/// promoted the item in the DB. The `beacon:` machine label is left alone
/// -- it identifies who did the work, not what stage it's in. Best-effort
/// like the rest of this module: a label failure here must never undo (or
/// even appear to block) a DB promotion that has already happened.
pub fn relabel_pr_completed(item: &agentflare_backend::item::Item, repo_root: &Path) {
    let branch = flare_git_core::worktree::task_branch_name(item);
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return;
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(_) => return,
    };
    let pr = match crate::github::pulls::find_existing(&client, &repo, &branch) {
        Ok(Some(pr)) => pr,
        Ok(None) => return,
        Err(e) => {
            eprintln!(
                "worktree: could not look up PR to relabel for item {}: {e}",
                item.id
            );
            return;
        }
    };
    if let Err(e) =
        crate::github::issues::remove_label(&client, &repo, pr.number, "agentflare:in-review")
    {
        eprintln!(
            "worktree: could not remove agentflare:in-review from PR #{}: {e}",
            pr.number
        );
    }
    if let Err(e) = crate::github::issues::add_labels(
        &client,
        &repo,
        pr.number,
        &["agentflare:completed".to_string()],
    ) {
        eprintln!(
            "worktree: could not add agentflare:completed to PR #{}: {e}",
            pr.number
        );
    }
}

/// CI signal the in-review sweep (`supervisor::run_review_sweep`, item #65)
/// polls per item: merged (promote), failing (self-repair), or nothing
/// actionable yet. `Unknown` covers every soft-fail case `is_pr_merged`
/// above also treats as "not merged yet" -- no credentials, no resolvable
/// remote, no PR found, or a lookup error -- since the caller's fallback is
/// simply to poll again next tick.
pub enum PrCiStatus {
    Merged,
    Failing(Vec<String>),
    Pending,
    Passing,
    Unknown,
}

/// Same "total>0 && not pending" gate `cli::git::wait_for_checks` polls on,
/// applied once instead of in a loop -- the sweep itself provides the retry
/// cadence across ticks.
pub fn pr_ci_status(item: &agentflare_backend::item::Item, repo_root: &Path) -> PrCiStatus {
    let branch = flare_git_core::worktree::task_branch_name(item);
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return PrCiStatus::Unknown;
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(_) => return PrCiStatus::Unknown,
    };
    let pr = match crate::github::pulls::find_existing(&client, &repo, &branch) {
        Ok(Some(pr)) => pr,
        Ok(None) => return PrCiStatus::Unknown,
        Err(e) => {
            eprintln!(
                "worktree: could not check PR status for item {}: {e}",
                item.id
            );
            return PrCiStatus::Unknown;
        }
    };
    if pr.merged_at.is_some() {
        return PrCiStatus::Merged;
    }
    let Some(sha) = pr.head.as_ref().map(|h| h.sha.clone()) else {
        return PrCiStatus::Unknown;
    };
    let checks = match crate::github::actions::list_check_runs(&client, &repo, &sha) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "worktree: could not fetch check status for item {}: {e}",
                item.id
            );
            return PrCiStatus::Unknown;
        }
    };
    let summary = crate::github::mcp::checks_wait_summary(&checks, 0);
    let total = summary["total_checks"].as_u64().unwrap_or(0);
    if total == 0 || summary["pending"].as_bool().unwrap_or(true) {
        return PrCiStatus::Pending;
    }
    let failed: Vec<String> = summary["failed_checks"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if failed.is_empty() {
        PrCiStatus::Passing
    } else {
        PrCiStatus::Failing(failed)
    }
}

/// The PR body: `summary` (the agent's own "what changed and why", or an
/// explicit `summary` on the `done` call) when it's real content, else the
/// old generic placeholder. A real summary makes for a far more reviewable
/// PR than the placeholder — reviewers previously had to open the diff
/// cold, with no idea what the change was even trying to do.
fn pr_body(item_id: &str, summary: Option<&str>) -> String {
    match summary.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => format!("Auto-opened on `item done` for {item_id}."),
    }
}

/// Human-readable attribution appended to the PR body -- who opened it and
/// on which machine, so a reviewer never has to guess whether a PR came
/// from agentflare. Unlike `bridge::marker::Marker`, this is not a
/// parseable format: nothing reads a PR footer back (the bridge doesn't
/// poll PRs for claim state the way it polls issues), so there is no
/// format to keep stable.
fn pr_footer(agent: &str, machine: &str, sequence_id: i64) -> String {
    format!("---\n_Opened by `{agent}` on **{machine}** for item #{sequence_id} via agentflare._")
}

/// Pushes `item`'s isolated worktree branch and opens a PR against
/// `target_branch` — the `done`-side counterpart to `create_worktree`.
/// Deliberately never merges: unreviewed code should never land on the
/// target branch automatically, so the worktree/branch are left in place
/// for the PR to actually get reviewed and merged. Soft-fails (eprintln, no
/// error surfaced, returns `None`) on any failure — nothing here, including
/// `gh`/GitHub credentials being unavailable, should block `done` since the
/// item's completion is already committed to the DB by the time this runs.
pub fn push_and_open_pr(
    item: &agentflare_backend::item::Item,
    agent: &str,
    repo_root: &Path,
    target_branch: &str,
    progress: Option<&ProgressSender>,
    summary: Option<&str>,
) -> Option<String> {
    let branch = flare_git_core::worktree::push_branch(
        item,
        repo_root,
        target_branch,
        as_progress(progress),
    )?;
    if let Some(p) = progress {
        p.send(0.5, Some(1.0), Some("Creating PR...".into()));
    }
    let machine = crate::github::bridge::config::machine_label();
    let body = format!(
        "{}\n\n{}",
        pr_body(&item.id, summary),
        pr_footer(agent, &machine, item.sequence_id)
    );
    let repo = match RepoId::resolve_from_remote(repo_root) {
        Some(r) => r,
        None => {
            eprintln!(
                "worktree: cannot resolve origin remote, skipping PR for item {}",
                item.id
            );
            return None;
        }
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "worktree: no GitHub credentials, skipping PR for item {}: {e}",
                item.id
            );
            return None;
        }
    };
    // Check for an existing PR on this branch before opening a new one.
    // GitHub's own API only rejects a duplicate while the existing PR is
    // still open -- once it's merged (or manually closed), a second PR
    // against the same branch is perfectly legal to create, which is
    // exactly how a `done` re-run on an already-merged item ended up
    // opening a redundant PR (2026-07-25). A lookup failure here is
    // soft-failed the same way the rest of this function is: log and fall
    // through to `create`, since a rare duplicate is a far smaller harm
    // than silently never opening a PR on a lookup hiccup.
    match crate::github::pulls::find_existing(&client, &repo, &branch) {
        Ok(Some(existing)) => {
            if let Some(p) = progress {
                p.send(1.0, Some(1.0), Some("PR already exists".into()));
            }
            return Some(existing.html_url);
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!(
                "worktree: could not check for an existing PR for item {}: {e} -- creating one anyway",
                item.id
            );
        }
    }
    match crate::github::pulls::create(
        &client,
        &repo,
        &item.name,
        &branch,
        target_branch,
        Some(&body),
    ) {
        Ok(pr) => {
            if let Err(e) = crate::github::issues::add_labels(
                &client,
                &repo,
                pr.number,
                &[
                    "agentflare:in-review".to_string(),
                    format!("beacon:{machine}"),
                ],
            ) {
                eprintln!(
                    "worktree: could not label PR #{} for item {}: {e}",
                    pr.number, item.id
                );
            }
            if let Some(p) = progress {
                p.send(1.0, Some(1.0), Some("PR created".into()));
            }
            Some(pr.html_url)
        }
        Err(e) => {
            eprintln!("worktree: PR creation failed for item {}: {e}", item.id);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_body_uses_the_summary_when_given_one() {
        assert_eq!(
            pr_body("item-1", Some("Fixed the race by adding a mutex.")),
            "Fixed the race by adding a mutex."
        );
    }

    #[test]
    fn pr_body_trims_the_summary() {
        assert_eq!(
            pr_body("item-1", Some("  Fixed the race.  \n")),
            "Fixed the race."
        );
    }

    #[test]
    fn pr_body_falls_back_to_the_placeholder_when_summary_is_none() {
        assert_eq!(
            pr_body("item-1", None),
            "Auto-opened on `item done` for item-1."
        );
    }

    #[test]
    fn pr_body_falls_back_to_the_placeholder_when_summary_is_blank() {
        assert_eq!(
            pr_body("item-1", Some("   ")),
            "Auto-opened on `item done` for item-1."
        );
    }

    #[test]
    fn pr_footer_names_agent_machine_and_item() {
        assert_eq!(
            pr_footer("claude-code", "kumar-laptop", 42),
            "---\n_Opened by `claude-code` on **kumar-laptop** for item #42 via agentflare._"
        );
    }

    #[test]
    fn relabel_pr_completed_is_a_noop_without_a_resolvable_remote() {
        let dir = tempfile::tempdir().unwrap();
        let item = agentflare_backend::item::Item {
            id: "item-1".into(),
            project_id: "p".into(),
            state_id: "s".into(),
            name: "n".into(),
            description: String::new(),
            priority: "none".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id: 9,
            sort_order: 0.0,
            started_at: None,
            completed_at: None,
            archived_at: None,
            external_source: None,
            external_id: None,
            metadata: "{}".into(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
        };
        // No git repo at `dir.path()`, so `RepoId::resolve_from_remote`
        // returns `None` -- must return without panicking.
        relabel_pr_completed(&item, dir.path());
    }
}
