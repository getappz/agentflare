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

/// Label on an item whose mass deletion is deliberate: the guards stand down.
pub const ALLOW_MASS_DELETION_LABEL: &str = "allow-mass-deletion";

/// More tracked files deleted at once than this is treated as a wipe...
const MAX_DELETED_FILES: usize = 20;
/// ...and so is more than this share of all tracked files, in a repo big
/// enough for a share to mean something.
const MAX_DELETED_PERCENT: usize = 25;
const MIN_TRACKED_FOR_PERCENT: usize = 4;

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
/// threshold; `Err` when git could not say how many are tracked.
fn mass_deletion(dir: &Path, rev: &str, deleted: usize) -> Result<Option<MassDeletion>, String> {
    if deleted == 0 {
        return Ok(None);
    }
    let tracked = run_git_in(dir, &["ls-tree", "-r", "--name-only", rev])?
        .lines()
        .count();
    let over_share =
        tracked >= MIN_TRACKED_FOR_PERCENT && deleted * 100 > tracked * MAX_DELETED_PERCENT;
    Ok((deleted > MAX_DELETED_FILES || over_share).then_some(MassDeletion { deleted, tracked }))
}

/// Uncommitted deletions in a worktree, and whether they are all it holds.
struct Uncommitted {
    deleted: usize,
    /// Every change is a tracked file missing from the working tree --
    /// nothing staged, modified or untracked that a restore could clobber
    /// or that marks the deletions as someone's deliberate work.
    only_unstaged_deletions: bool,
}

fn uncommitted(worktree_path: &Path) -> Result<Option<Uncommitted>, String> {
    // Run in a directory that isn't its own checkout, `status` would
    // describe the enclosing main repository instead.
    if !is_own_checkout(worktree_path) {
        return Ok(None);
    }
    // v2, not v1: `run_in` trims its output, which would eat the leading
    // space of a v1 ` D path` first line and turn it into a staged `D `.
    let status = run_git_in(worktree_path, &["status", "--porcelain=v2"])?;
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
    Ok(Some(state))
}

/// `Some` when the working tree at `worktree_path` is missing more of
/// `HEAD`'s tracked files than a deliberate change plausibly deletes --
/// checked before anything commits that tree. `Ok(None)` is "checked, clean"
/// (or not a checkout of its own); `Err` is "git could not tell".
pub fn worktree_mass_deletion(worktree_path: &Path) -> Result<Option<MassDeletion>, String> {
    match uncommitted(worktree_path)? {
        Some(state) => mass_deletion(worktree_path, "HEAD", state.deleted),
        None => Ok(None),
    }
}

/// `Some` when pushing `branch` would publish a mass deletion: either its
/// tip commit alone, or everything it adds on top of `target_branch`
/// (a wipe committed earlier and built upon). Renames are not deletions.
/// `Ok(None)` when the branch has no commits of its own.
///
/// The fork point is taken against both the local target and
/// `origin/<target>` -- `item claim` rebases onto the remote while the local
/// target can sit stale, and everything upstream deleted since would read as
/// the branch's own. Of the forks, the most recent one counts. Any git
/// failure is an `Err`: a guard that cannot look must not wave a push through.
pub fn branch_mass_deletion(
    repo_root: &Path,
    branch: &str,
    target_branch: &str,
) -> Result<Option<MassDeletion>, String> {
    let git = |args: &[&str]| run_git_in(repo_root, args);
    let tip = git(&["rev-parse", "--verify", branch])?;
    let remote = format!("refs/remotes/origin/{target_branch}");
    let mut forks = Vec::new();
    for target in [target_branch, remote.as_str()] {
        if git(&["rev-parse", "--verify", "--quiet", target]).is_ok() {
            forks.push(git(&["merge-base", target, branch])?);
        }
    }
    if forks.is_empty() {
        return Err(format!(
            "neither `{target_branch}` nor `origin/{target_branch}` exists to compare `{branch}` against"
        ));
    }
    if forks.contains(&tip) {
        return Ok(None);
    }
    let mut by_distance = Vec::new();
    for fork in forks {
        let ahead: usize = git(&["rev-list", "--count", &format!("{fork}..{branch}")])?
            .parse()
            .map_err(|e| format!("unreadable rev-list count: {e}"))?;
        by_distance.push((ahead, fork));
    }
    let mut bases: Vec<String> = by_distance
        .into_iter()
        .min_by_key(|(ahead, _)| *ahead)
        .map(|(_, fork)| fork)
        .into_iter()
        .collect();
    // The tip alone, but only for a plain commit: a merge tip's first-parent
    // diff holds everything the merged-in side deleted.
    if git(&["rev-list", "--parents", "-n1", &tip])?
        .split_whitespace()
        .count()
        == 2
    {
        bases.insert(0, format!("{branch}~1"));
    }
    for base in bases {
        let deleted = git(&[
            "diff",
            "-M",
            "--name-only",
            "--diff-filter=D",
            &base,
            branch,
        ])?
        .lines()
        .count();
        if let Some(wipe) = mass_deletion(repo_root, &base, deleted)? {
            return Ok(Some(wipe));
        }
    }
    Ok(None)
}

/// Sanity check for a dispatch into an existing worktree: when its tracked
/// files were mass-deleted and that is ALL that changed, puts them back
/// from `HEAD` and fails the dispatch to say so. Anything else alongside the deletions (an edit, a staged
/// change, an untracked file) means a restore could clobber or contradict
/// real work, so the dispatch fails instead and nothing is touched.
pub(super) fn restore_mass_deleted_worktree(worktree_path: &Path) -> Result<(), String> {
    let Some(state) = uncommitted(worktree_path)? else {
        return Ok(());
    };
    let Some(wipe) = mass_deletion(worktree_path, "HEAD", state.deleted)? else {
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
    // An `Err`, not a silent success: otherwise an agent that deleted the
    // files on purpose redoes it after every redispatch, forever.
    Err(format!(
        "worktree: {wipe} in {at} -- restored the tracked files from HEAD. If the deletion was \
         deliberate, label the item `{ALLOW_MASS_DELETION_LABEL}` (or finish with `item done \
         force=true` and a force_reason) and dispatch again"
    ))
}
