//! Mass-deletion ("wipe") detection for item worktrees (item #689). A
//! worktree hollowed out by a failed removal looked, to everything
//! downstream, like an agent that had deleted every tracked file: the SDD
//! checkpoint committed it and `item done` would have pushed it. Child
//! module of `worktree`; public items are re-exported from there.

use std::path::Path;

use super::is_own_checkout;
use crate::shell::run_in as run_git_in;

/// Phrase every mass-deletion refusal carries.
pub const MASS_DELETION_MARKER: &str = "mass deletion";

/// More tracked files deleted at once than this is treated as a wipe...
const MAX_DELETED_FILES: usize = 20;
/// ...and so is more than this share of all tracked files.
const MAX_DELETED_PERCENT: usize = 25;

/// How many tracked files a change deletes, out of how many were tracked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MassDeletion {
    pub deleted: usize,
    pub tracked: usize,
}

impl std::fmt::Display for MassDeletion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{MASS_DELETION_MARKER}: {} of {} tracked files deleted",
            self.deleted, self.tracked
        )
    }
}

/// `Some` when `deleted` files out of those tracked at `rev` is over either
/// threshold.
fn mass_deletion(dir: &Path, rev: &str, deleted: usize) -> Option<MassDeletion> {
    if deleted == 0 {
        return None;
    }
    let tracked = run_git_in(dir, &["ls-tree", "-r", "--name-only", rev])
        .ok()?
        .lines()
        .count();
    (deleted > MAX_DELETED_FILES || deleted * 100 > tracked * MAX_DELETED_PERCENT)
        .then_some(MassDeletion { deleted, tracked })
}

/// Uncommitted deletions in a worktree, and whether they are all it holds.
struct Uncommitted {
    deleted: usize,
    /// Every change is a tracked file missing from the working tree --
    /// nothing staged, modified or untracked that a restore could clobber
    /// or that marks the deletions as someone's deliberate work.
    only_unstaged_deletions: bool,
}

fn uncommitted(worktree_path: &Path) -> Option<Uncommitted> {
    // Run in a directory that isn't its own checkout, `status` would
    // describe the enclosing main repository instead.
    if !is_own_checkout(worktree_path) {
        return None;
    }
    // v2, not v1: `run_in` trims its output, which would eat the leading
    // space of a v1 ` D path` first line and turn it into a staged `D `.
    let status = run_git_in(worktree_path, &["status", "--porcelain=v2"]).ok()?;
    let mut state = Uncommitted {
        deleted: 0,
        only_unstaged_deletions: true,
    };
    for line in status.lines() {
        match line.strip_prefix("1 ").and_then(|rest| rest.get(..2)) {
            Some(".D") => state.deleted += 1,
            Some(xy) if xy.contains('D') => {
                state.deleted += 1;
                state.only_unstaged_deletions = false;
            }
            _ => state.only_unstaged_deletions = false,
        }
    }
    Some(state)
}

/// `Some` when the working tree at `worktree_path` is missing more of
/// `HEAD`'s tracked files than a deliberate change plausibly deletes --
/// checked before anything commits that tree.
#[must_use]
pub fn worktree_mass_deletion(worktree_path: &Path) -> Option<MassDeletion> {
    let state = uncommitted(worktree_path)?;
    mass_deletion(worktree_path, "HEAD", state.deleted)
}

/// `Some` when pushing `branch` would publish a mass deletion: either its
/// tip commit alone, or everything it adds on top of `target_branch`
/// (a wipe committed earlier and built upon). Renames are not deletions.
/// `None` when the branch has no commits of its own.
#[must_use]
pub fn branch_mass_deletion(
    repo_root: &Path,
    branch: &str,
    target_branch: &str,
) -> Option<MassDeletion> {
    let fork = run_git_in(repo_root, &["merge-base", target_branch, branch]).ok();
    if fork == run_git_in(repo_root, &["rev-parse", branch]).ok() {
        return None;
    }
    std::iter::once(format!("{branch}~1"))
        .chain(fork)
        .find_map(|base| {
            let deleted = run_git_in(
                repo_root,
                &[
                    "diff",
                    "-M",
                    "--name-only",
                    "--diff-filter=D",
                    &base,
                    branch,
                ],
            )
            .ok()?
            .lines()
            .count();
            mass_deletion(repo_root, &base, deleted)
        })
}

/// Sanity check for a dispatch into an existing worktree: when its tracked
/// files were mass-deleted and that is ALL that changed, puts them back
/// from `HEAD`. Anything else alongside the deletions (an edit, a staged
/// change, an untracked file) means a restore could clobber or contradict
/// real work, so the dispatch fails instead and nothing is touched.
pub(super) fn restore_mass_deleted_worktree(worktree_path: &Path) -> Result<(), String> {
    let Some(state) = uncommitted(worktree_path) else {
        return Ok(());
    };
    let Some(wipe) = mass_deletion(worktree_path, "HEAD", state.deleted) else {
        return Ok(());
    };
    let at = worktree_path.display();
    if !state.only_unstaged_deletions {
        return Err(format!(
            "worktree: {wipe} in {at} alongside other uncommitted changes -- refusing to \
             dispatch into it. Inspect it by hand; `git restore --worktree --source=HEAD -- .` \
             brings the tracked files back"
        ));
    }
    run_git_in(
        worktree_path,
        &["restore", "--worktree", "--source=HEAD", "--", "."],
    )
    .map_err(|e| format!("worktree: {wipe} in {at}, and restoring it from HEAD failed: {e}"))?;
    eprintln!("worktree: {wipe} in {at} -- restored the tracked files from HEAD");
    Ok(())
}
