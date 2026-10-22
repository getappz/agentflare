//! `done`'s "the PR I tracked has already merged" path (item #373).
//!
//! Split out of `item.rs` purely to keep that file under the repo's
//! line-count gate (`scripts/loc-gate.sh`); there is no dependency boundary
//! here beyond `super`.

use super::*;

/// Completes the item when the PR it tracked has already merged.
///
/// A human (or another instance's finalize) can merge an item's PR while its
/// job is still running, and the head branch goes with it. `done` then tries
/// to push a branch that no longer exists, and the item is left wedged in
/// "started" with its PR record stripped -- a state nothing could recover
/// from, because the sweep's self-heal reads the PR number out of that same
/// record (item #283 / PR #910, item #173 / PR #666). This path recognises
/// the merge and completes the item instead of pushing.
///
/// Two conditions keep it honest:
///
/// - it keys on `metadata.pr`, and `redispatch` clears that (see
///   `crates/agentflare-backend/src/item/claim.rs`), so an item handed back
///   for more work carries no PR reference and can never be completed by
///   someone else's merge;
/// - the merge has to have covered the branch. A branch tip that moved on
///   after the merge holds work the PR never carried, and that work must not
///   be completed away.
///
/// Returns the response to answer `done` with, or `None` when the PR has not
/// merged -- the caller then carries on with the normal push path.
pub(crate) fn complete_if_pr_already_merged(
    mcp: &AgentflareMcp,
    item_id: &str,
    item: &Option<agentflare_backend::item::Item>,
    repo_root: &std::path::Path,
    owner: &str,
    now: i64,
) -> Result<Option<serde_json::Value>, ErrorData> {
    let Some(pr) = merged_pr_for(mcp, item, repo_root) else {
        return Ok(None);
    };
    let moved = mcp.with_backend_db(|conn| {
        agentflare_backend::item::mark_completed(conn, item_id, owner).map_err(map_backend_err)
    })??;
    if moved {
        release_claim_and_cleanup(mcp, item_id, item, repo_root, owner, now);
    }
    Ok(Some(serde_json::json!({
        "done": moved,
        "item_id": item_id,
        "status": if moved { "completed" } else { "unchanged" },
        "promoted": true,
        "pr_number": pr.number,
    })))
}

/// The item's tracked PR, when it has already merged **and** that merge
/// covered the item's branch.
fn merged_pr_for(
    mcp: &AgentflareMcp,
    item: &Option<agentflare_backend::item::Item>,
    repo_root: &std::path::Path,
) -> Option<crate::worktree::MergedPr> {
    let item = item.as_ref()?;
    let client = mcp.github_client()?;
    let repo = crate::github::RepoId::resolve_from_remote(repo_root)?;
    let pr = crate::worktree::merged_pr(item, repo_root, &client, &repo)?;
    let branch = flare_git_core::worktree::resolve_item_task_branch(item, repo_root);
    crate::worktree::branch_is_covered_by_merged_pr(repo_root, &branch, pr.head_sha.as_deref())
        .then_some(pr)
}

/// Releases the claim lease and removes the item's worktree.
///
/// Shared by both "nothing was ever committed" (the worktree is clean by
/// definition, safe to remove) and a real completion: the lease has to go so
/// the item is genuinely available again -- either for a fresh claim (nothing
/// done) or because the work is done -- instead of leaving it wedged on a
/// claim nobody will ever release. `cleanup_worktree` itself still verifies
/// the tree is clean before removing it, so this is safe even if the caller
/// somehow raced with an uncommitted local change.
pub(crate) fn release_claim_and_cleanup(
    mcp: &AgentflareMcp,
    item_id: &str,
    item: &Option<agentflare_backend::item::Item>,
    repo_root: &std::path::Path,
    owner: &str,
    now: i64,
) {
    if let Some(item) = item {
        crate::worktree::cleanup_worktree(item, repo_root);
    }
    match mcp.with_backend_db(|conn| agentflare_backend::claim::done(conn, item_id, owner, now)) {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => eprintln!(
            "worktree: releasing claim for item {item_id} affected no rows (owner mismatch or already released)"
        ),
        Ok(Err(e)) => {
            eprintln!("worktree: failed to release claim for item {item_id}: {e}")
        }
        Err(e) => {
            eprintln!("worktree: failed to release claim for item {item_id}: {e:?}")
        }
    }
}
