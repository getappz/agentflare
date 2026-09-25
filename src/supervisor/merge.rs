//! The supervisor's PR merge / promotion path: merging a CI-green, approved
//! PR (`handle_ci_green`, `merge_if_approved`, `merge_approved_pr`),
//! promoting the item once its PR has merged (`promote_merged_item`,
//! `delete_merged_head_branch`), and requeueing an item whose PR was closed
//! unmerged (`requeue_closed_pr_item`). Split out of `supervisor.rs` to keep
//! it under the LOC gate (`scripts/loc-gate.sh`); re-exported by the parent
//! as `use merge::*`, so `supervisor_tests.rs`'s `use super::*` is unchanged.

use super::*;

/// Whether a CI-green PR may be merged this tick, and at which head.
#[derive(Clone, Copy, Debug)]
pub(super) enum CiGreenMerge<'a> {
    /// Merge once approved, pinned to `head_sha` -- the commit the green
    /// verdict was made on.
    Allowed { head_sha: Option<&'a str> },
    /// GitHub is blocking the merge until a human review lands; don't try.
    BlockedOnReview,
}

/// `handle_pr_status`'s CI-green path, shared by `Passing` and
/// `AwaitingReview` -- the two differ only in whether a merge is attempted.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_ci_green(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    auth_conn: &rusqlite::Connection,
    host_policy: agentflare_resource_gate::Policy,
    item: &agentflare_backend::item::Item,
    number: u64,
    labels: &[String],
    merge: CiGreenMerge<'_>,
    label_id_by_name: &std::collections::HashMap<String, String>,
    folder_path: &str,
    repo_root: &std::path::Path,
    result: &mut ReviewSweepResult,
) {
    // CI just went green -- if the PR was still carrying a
    // self-repair/needs-human stage label from before, swap it back
    // to plain in-review rather than leaving a stale "under repair"
    // label on a now-passing PR. `labels` is already in hand from
    // the batched/single fetch above, so this only touches GitHub
    // when there's actually something to revert.
    if let Some(stale) = [SELF_REPAIR_PR_LABEL, NEEDS_HUMAN_PR_LABEL]
        .into_iter()
        .find(|l| labels.iter().any(|have| have == l))
    {
        update_pr_stage(
            folder_path,
            number,
            Some(stale),
            IN_REVIEW_PR_LABEL,
            "## supervisor — CI green\n\nChecks are passing again.",
        );
    }
    // Item #303: post the one-time completion summary for whichever
    // `self_repair_or_gate` trigger(s) were previously announced on
    // this item -- each call is a no-op unless that trigger actually
    // has an unresolved announcement recorded, so it's safe to check
    // both unconditionally rather than trying to infer from the PR
    // label above which trigger (if either) was in play.
    maybe_post_repair_complete_summary(
        mcp,
        item,
        CI_SELF_REPAIR_MARKER,
        CI_SELF_REPAIR_COMPLETE_MARKER,
        "CI checks are passing again",
        CI_SELF_REPAIR_ANNOUNCED_KEY,
        CI_SELF_REPAIR_SILENT_KEY,
        CI_SELF_REPAIR_COMPLETED_KEY,
    );
    maybe_post_repair_complete_summary(
        mcp,
        item,
        CONFLICT_REPAIR_MARKER,
        CONFLICT_REPAIR_COMPLETE_MARKER,
        "The merge conflict is resolved",
        CONFLICT_REPAIR_ANNOUNCED_KEY,
        CONFLICT_REPAIR_SILENT_KEY,
        CONFLICT_REPAIR_COMPLETED_KEY,
    );
    // Namespaced ("pr-approval:<id>", not the bare item id): the
    // underlying set is keyed globally across every gate type in
    // this file (see `dispatch_item`'s "plan:" comment) -- an
    // unnamespaced key here silently starves this card of its
    // once-per-gate notify if the item was already gated for an
    // unrelated reason earlier in its life (e.g. the go/no-go
    // decision gate below, or `skip_item`), since that gate's call
    // already consumed the bare-id token (item #587).
    if !labels.iter().any(|l| l == PR_APPROVAL_LABEL)
        && first_time_gated(&format!("pr-approval:{}", item.id))
    {
        notify_pr_approval_gate(item, folder_path, number);
    }
    // CI being green and a human's approval label being attached
    // don't mean the PR is actually done if CodeRabbit's own review
    // still has unresolved findings sitting on it untouched (item
    // #273) -- so the findings check must run, and gate the merge,
    // *before* `merge_if_approved` is ever called, not only in the
    // branch where it happened not to merge (item #628: an approved,
    // CI-green PR with real findings on it got merged untouched
    // because the two were checked in the wrong order -- see GitHub
    // PR 791). Skip the two live GitHub calls this fetch costs
    // entirely once the item is already gated or a repair job is
    // already in flight -- `coderabbit_repair_or_gate` would just
    // discard the findings and return `Skipped` anyway, but not
    // before paying for the fetch on every single tick for as long
    // as the PR sits gated or in-flight.
    if already_gated_or_in_flight(mcp, queue, item, label_id_by_name) {
        result.skipped += 1;
    } else {
        let head_sha = match merge {
            CiGreenMerge::Allowed { head_sha } => head_sha,
            CiGreenMerge::BlockedOnReview => None,
        };
        let review = fetch_review_bot_state(
            mcp,
            queue,
            item,
            repo_root,
            number,
            head_sha,
            labels,
            label_id_by_name,
            folder_path,
        );
        match merge_or_repair_findings(
            mcp,
            queue,
            auth_conn,
            host_policy,
            item,
            repo_root,
            number,
            &review,
            labels,
            label_id_by_name,
            folder_path,
            merge,
        ) {
            PassingPrOutcome::Merged => result.promoted += 1,
            PassingPrOutcome::NotMerged => match merge {
                CiGreenMerge::Allowed { .. } => result.skipped += 1,
                CiGreenMerge::BlockedOnReview => result.waiting += 1,
            },
            PassingPrOutcome::Repair(SelfRepairOutcome::Dispatched) => result.review_repaired += 1,
            PassingPrOutcome::Repair(SelfRepairOutcome::Deferred) => result.waiting += 1,
            PassingPrOutcome::Repair(SelfRepairOutcome::Skipped) => result.skipped += 1,
        }
    }
}

/// A PR closed without merging will never land, and nothing in the sweep
/// would ever move its item out of "in_review" again -- it used to sit there
/// forever, holding its claim lease. Sends the item back for a fresh attempt
/// instead: `item::redispatch` resets it to "backlog", re-attaches
/// `ready-for-work` and clears `metadata.pr` (so the stray-PR self-heal
/// can't drag it straight back), then the abandoned attempt's lease is
/// released so the next dispatch can claim it. One comment records why.
///
/// Re-checks the item's live state first: the sweep's snapshot can be stale,
/// and the state move out of "in_review" is what makes this run once per
/// closed PR rather than once per tick. Returns whether the item was moved.
pub(super) fn requeue_closed_pr_item(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    number: u64,
) -> bool {
    let author = crate::claims::owner_id();
    let outcome = mcp.with_backend_db(|conn| -> agentflare_backend::error::Result<bool> {
        let current = agentflare_backend::item::get(conn, &item.id)?;
        let state = agentflare_backend::state::get(conn, &current.state_id)?;
        if state.group_name != "in_review" {
            return Ok(false);
        }
        // A different PR recorded since the snapshot was taken is not the
        // one that was just seen closed.
        if crate::worktree::pr_number_from_metadata(&current).is_some_and(|n| n != number) {
            return Ok(false);
        }
        let rearmed = match agentflare_backend::item::redispatch(conn, &item.id, None)? {
            agentflare_backend::item::RedispatchOutcome::Ready { .. } => true,
            agentflare_backend::item::RedispatchOutcome::NoAssignee => {
                // Nobody to hand it back to automatically: still leave
                // "in_review" and drop the dead PR, so it waits in the
                // backlog for an assignee instead of polling a closed PR.
                let backlog =
                    agentflare_backend::state::first_in_group(conn, &current.project_id, "backlog")?;
                agentflare_backend::item::update_state(conn, &item.id, &backlog.id)?;
                conn.execute(
                    "UPDATE items SET metadata = json_remove(metadata, '$.pr') \
                     WHERE id = ?1 AND json_valid(metadata)",
                    rusqlite::params![item.id],
                )?;
                false
            }
        };
        if let Some(owner) = agentflare_backend::claim::current_owner(conn, &item.id) {
            agentflare_backend::claim::release(conn, &item.id, &owner)?;
        }
        let next = if rearmed {
            "It has been moved back to the backlog and re-armed with `ready-for-work` for a fresh attempt."
        } else {
            "It has been moved back to the backlog; it has no assignee, so assign one to have it picked up again."
        };
        agentflare_backend::comment::create(
            conn,
            &item.id,
            &author,
            &format!(
                "## supervisor — PR closed without merging\n\nPR #{number} was closed without \
                 being merged, so nothing from it will land. {next}"
            ),
        )?;
        Ok(true)
    });
    match outcome {
        Ok(Ok(moved)) => moved,
        Ok(Err(e)) => {
            eprintln!(
                "agentflare-supervisor: could not requeue item #{} ({}) after its PR #{number} \
                 was closed: {e}",
                item.sequence_id, item.id
            );
            false
        }
        Err(e) => {
            eprintln!(
                "agentflare-supervisor: could not requeue item #{} ({}) after its PR #{number} \
                 was closed: {e:?}",
                item.sequence_id, item.id
            );
            false
        }
    }
}

/// Item #234's self-heal (`run_review_sweep`'s `stray_candidates` handling):
/// confirms a stray item's tracked PR hasn't simply been closed without
/// merging before restoring the item to "in_review" -- an abandoned PR is
/// the one case metadata presence and claim liveness alone can't rule out.
/// Split out from the sweep's loop, mirroring `merge_approved_pr`'s own
/// test seam, so tests can drive it against a mock server instead of
/// `Client::new()`'s real credentials/host -- `run_review_sweep`'s own
/// integration tests never touch the network at all (see
/// `throwaway_repo`'s doc comment), so this is the only way to pin the
/// actual open/merged/closed decision.
pub(super) fn stray_pr_is_still_relevant(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    number: u64,
) -> bool {
    match crate::github::pulls::get(client, repo, number) {
        Ok(pr) => pr.state != "closed" || pr.merged_at.is_some(),
        Err(_) => false,
    }
}

pub(super) fn promote_merged_item(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
) -> bool {
    let Ok(json) = mcp.item_check_merge(ItemRequest {
        action: "check_merge".into(),
        id: Some(item.id.clone()),
        ..Default::default()
    }) else {
        return false;
    };
    let promoted = serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|v| v["promoted"].as_bool())
        .unwrap_or(false);
    if promoted {
        delete_merged_head_branch(item, repo_root);
    }
    promoted
}

/// Branch hygiene once a merged item is promoted: deletes the PR's head
/// branch on GitHub so merged `task/<n>` branches don't pile up on the
/// remote. `github::repos::delete_merged_pr_branch` refuses anything but a
/// merged, same-repo, unprotected, non-default branch, and defers to the
/// repo's own `delete_branch_on_merge`. Remote only -- the local branch and
/// worktree belong to worktree cleanup. Soft-fails: a leftover branch is
/// untidy, never a reason to fail the promotion that already happened.
/// Items without a recorded `metadata.pr.number` are skipped rather than
/// guessed at by branch name.
pub(super) fn delete_merged_head_branch(
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
) {
    let Some(number) = crate::worktree::pr_number_from_metadata(item) else {
        return;
    };
    let Some(repo) = crate::github::RepoId::resolve_from_remote(repo_root) else {
        return;
    };
    let Ok(client) = crate::github::Client::new() else {
        return;
    };
    delete_merged_head_branch_with(&client, &repo, number);
}

/// `delete_merged_head_branch`'s GitHub half, split out for mock-server
/// tests the same way `merge_approved_pr` is.
pub(super) fn delete_merged_head_branch_with(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    number: u64,
) {
    match crate::github::repos::delete_merged_pr_branch(client, repo, number) {
        Ok(crate::github::repos::BranchCleanup::Deleted) => {
            eprintln!(
                "agentflare-supervisor: deleted merged head branch of PR #{number} in {repo}"
            );
        }
        Ok(crate::github::repos::BranchCleanup::Advanced) => {
            eprintln!(
                "agentflare-supervisor: kept head branch of merged PR #{number} in {repo} -- \
                 it has commits pushed after the merge"
            );
        }
        Ok(crate::github::repos::BranchCleanup::AtomicDeleteUnavailable(why)) => {
            eprintln!(
                "agentflare-supervisor: kept head branch of merged PR #{number} in {repo} -- \
                 atomic (sha-guarded) ref deletion unavailable ({why}); not falling back to \
                 an unguarded delete"
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!(
            "agentflare-supervisor: could not delete merged head branch of PR #{number} in {repo}: {}",
            e.log_safe()
        ),
    }
}

/// Routes a CI-green PR to either a review-bot repair dispatch or an
/// approval-gated merge attempt -- `review` (the sweep of the PR's bot
/// threads, pre-fetched by the caller, same convention as
/// `self_repair_or_gate`'s `failed_checks`) is checked FIRST, so a PR with
/// unresolved bot findings can never reach `merge_if_approved`, regardless
/// of its approval label or CI status (item #628: the two were previously
/// checked in the wrong order -- `merge_if_approved` ran first and findings
/// were only checked in the branch where it did NOT merge -- so an approved,
/// CI-green PR with real findings still sitting on it got merged untouched;
/// see GitHub PR 791). Findings the agent is to work on are dispatched when
/// any of them is non-optional (nitpicks alone never start a repair or hold
/// the merge); a non-optional thread still open for any other reason
/// (waiting on the bot to accept a reply, a fix not yet pushed, escalated
/// to a human) holds the merge without a dispatch.
#[allow(clippy::too_many_arguments)]
pub(super) fn merge_or_repair_findings(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    auth_conn: &rusqlite::Connection,
    host_policy: agentflare_resource_gate::Policy,
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
    number: u64,
    review: &ReviewBotState,
    labels: &[String],
    label_id_by_name: &std::collections::HashMap<String, String>,
    folder_path: &str,
    merge: CiGreenMerge<'_>,
) -> PassingPrOutcome {
    if review.dispatch_needed() {
        return PassingPrOutcome::Repair(coderabbit_repair_or_gate(
            mcp,
            queue,
            auth_conn,
            host_policy,
            item,
            number,
            &review.to_dispatch,
            labels,
            label_id_by_name,
            folder_path,
        ));
    }
    if review.blocks_merge() {
        return PassingPrOutcome::NotMerged;
    }
    let summary = maybe_post_repair_complete_summary(
        mcp,
        item,
        CODERABBIT_REPAIR_MARKER,
        CODERABBIT_REPAIR_COMPLETE_MARKER,
        "All CodeRabbit findings are resolved",
        CODERABBIT_REPAIR_ANNOUNCED_KEY,
        CODERABBIT_REPAIR_SILENT_KEY,
        CODERABBIT_REPAIR_COMPLETED_KEY,
    );
    clear_stale_coderabbit_repair_label(folder_path, number, labels, summary.as_deref());
    let CiGreenMerge::Allowed { head_sha } = merge else {
        return PassingPrOutcome::NotMerged;
    };
    if merge_if_approved(mcp, item, repo_root, number, labels, head_sha) {
        PassingPrOutcome::Merged
    } else {
        PassingPrOutcome::NotMerged
    }
}

/// Auto-merges a CI-green PR and promotes its item, but only once a human
/// has attached `PR_APPROVAL_LABEL` to the PR itself -- checked first and
/// short-circuits before any GitHub call so an unapproved item never touches
/// the network here. Only ever called from `merge_or_repair_findings`, once
/// it has confirmed there are no unresolved CodeRabbit findings, so CI green
/// is structurally required and findings are structurally clean: the label
/// can add a gate on top of both, never bypass either.
pub(super) fn merge_if_approved(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
    number: u64,
    labels: &[String],
    head_sha: Option<&str>,
) -> bool {
    if !labels.iter().any(|l| l == PR_APPROVAL_LABEL) {
        return false;
    }
    let Some(repo) = crate::github::RepoId::resolve_from_remote(repo_root) else {
        return false;
    };
    let Ok(client) = crate::github::Client::new() else {
        return false;
    };
    merge_approved_pr(&client, &repo, number, head_sha) && promote_merged_item(mcp, item, repo_root)
}

/// The actual GitHub merge call for an approved, CI-green PR. Split out from
/// `merge_if_approved` so tests can drive it against a mock server instead
/// of `Client::new()`'s real credentials/host, mirroring `github::pulls`'
/// own test style. Squash matches this repo's existing single-commit-per-item
/// convention. Logs and falls through (never retries in-line) on failure --
/// branch protection or a merge conflict just means the item sits until the
/// next sweep tick, same as any other `skipped` outcome.
///
/// Pinned to `head_sha`, the commit the sweep's snapshot judged CI-green: the
/// sweep acts on a snapshot, and an agent may push between the fetch and
/// this call. GitHub then answers 409 instead of merging unchecked code,
/// which is an ordinary skip -- the next tick judges the new head.
pub(super) fn merge_approved_pr(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    number: u64,
    head_sha: Option<&str>,
) -> bool {
    match crate::github::pulls::merge_at_head(client, repo, number, "squash", head_sha) {
        Ok(()) => true,
        Err(e) if crate::github::pulls::is_head_moved(&e) => {
            eprintln!(
                "agentflare-supervisor: PR #{number} in {repo} got new commits since CI was \
                 checked; not merging this tick"
            );
            false
        }
        Err(e) => {
            eprintln!(
                "agentflare-supervisor: auto-merge failed for PR #{number} in {repo}: {}",
                e.log_safe()
            );
            false
        }
    }
}
