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

pub fn create_worktree(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    target_branch: &str,
    progress: Option<&ProgressSender>,
) -> Result<PathBuf, String> {
    flare_git_core::worktree::create_worktree(item, repo_root, target_branch, as_progress(progress))
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
    repo_root: &Path,
    target_branch: &str,
    progress: Option<&ProgressSender>,
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
    let body = format!("Auto-opened on `item done` for {}.", item.id);
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
    match crate::github::pulls::create(
        &client,
        &repo,
        &item.name,
        &branch,
        target_branch,
        Some(&body),
    ) {
        Ok(pr) => {
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
