/// Duplicate-work pre-check (item #164): searches GitHub for a PR that
/// already carries `item`'s `for item #<sequence_id>` marker (the footer
/// `worktree::pr_footer` stamps onto every PR agentflare opens). Unlike
/// `worktree::is_pr_merged`, this doesn't depend on the item's own tracked
/// branch name still matching — it's what catches a PR that merged while
/// the item's tracked state fell out of sync (items #122/#156: the state
/// promotion never ran, so a routine redispatch nearly re-did already-
/// merged work).
///
/// Best-effort: no resolvable remote, no GitHub credentials, or a search
/// API error all just return `None` so dispatch proceeds normally — same
/// fail-open contract `worktree::is_pr_merged`/`relabel_pr_completed`
/// already use. Prefers a merged match over a still-open one (sorted first)
/// so `handle_duplicate_pr` can self-heal instead of merely flagging for
/// review, on the rare chance both exist.
fn find_duplicate_pr(
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
) -> Option<crate::github::models::PullRequest> {
    let repo = crate::github::RepoId::resolve_from_remote(repo_root)?;
    let client = crate::github::Client::new().ok()?;
    let prs = crate::github::pulls::find_by_item_marker(&client, &repo, item.sequence_id).ok()?;
    pick_duplicate_pr(prs, flare_git_core::branch::current_branch(repo_root).as_deref())
}

/// Picks which match to act on when `find_by_item_marker` returns more than
/// one PR for the same item — a merged one wins so `handle_duplicate_pr`
/// self-heals instead of merely flagging for review. A closed-but-unmerged
/// PR (a previously abandoned attempt, e.g. an item found superseded and
/// closed) is dropped entirely rather than treated as an open duplicate —
/// otherwise it would permanently block a legitimate redispatch. Split out
/// from `find_duplicate_pr` so this selection logic is unit-testable
/// without a mock GitHub server.
///
/// `current_branch` excludes a still-open PR that's already checked out in
/// *this* worktree from counting as a duplicate at all — otherwise a
/// self-repair job reclaiming its own item's existing PR (`self_repair_or_gate`
/// in `supervisor.rs`, whose whole point is to push a fix onto that exact
/// branch) finds its own PR via `find_by_item_marker`, treats it as
/// somebody else's competing duplicate, and bails without ever attempting
/// the repair — then, because `item::release` only clears the claim and
/// never restores the state group, the item is left orphaned in `started`
/// with no label either `run_discovery_tick` or `run_review_sweep` will
/// ever pick back up (reproduced live on item #186/PR #597). A *merged*
/// match still always short-circuits regardless of branch, since that's
/// this function's other job: self-heal an item whose PR landed while its
/// tracked state fell out of sync (items #122/#156).
fn pick_duplicate_pr(
    mut prs: Vec<crate::github::models::PullRequest>,
    current_branch: Option<&str>,
) -> Option<crate::github::models::PullRequest> {
    prs.retain(|pr| pr.merged_at.is_some() || pr.state == "open");
    prs.retain(|pr| {
        pr.merged_at.is_some()
            || current_branch
                .zip(pr.head.as_ref())
                .is_none_or(|(current, head)| head.git_ref != current)
    });
    prs.sort_by_key(|pr| pr.merged_at.is_none());
    prs.into_iter().next()
}

/// Handles a duplicate PR `find_duplicate_pr` already found, instead of
/// letting `execute_work_impl` dispatch an agent to redo (or re-review)
/// work that's already landed or already pending. Both branches are
/// terminal successes, not failures — matching how `work_item_pipeline`'s
/// `finalize` step treats its own "hold"/"needs human review" diversions.
///
/// - Merged: self-heal — mark the item completed, clean up its worktree,
///   relabel the PR `agentflare:completed`, and release the claim, the same
///   shape `item_check_merge`'s promoted path already uses. If marking
///   completed doesn't confirm success, this returns a retryable failure
///   instead of reporting success, leaving `claim_guard` armed so its
///   `Drop` releases the claim for a future attempt.
/// - Still open: skip dispatch and flag for a human instead of racing a
///   second PR for the same item.
///
/// Either way, `claim_guard` is only disarmed once `item_release` actually
/// confirms success — a failed release leaves it armed so `Drop`'s
/// best-effort retry is still the backstop, matching `release_and_comment`'s
/// own contract.
#[allow(clippy::too_many_arguments)]
fn handle_duplicate_pr(
    mcp: &AgentflareMcp,
    item_id: &str,
    item: &agentflare_backend::item::Item,
    worktree_path: &std::path::Path,
    pr: &crate::github::models::PullRequest,
    notify_recipient: Option<&str>,
    claim_guard: &mut ClaimGuard,
    log: &mut dyn std::io::Write,
) -> WorkOutcome {
    let body = if pr.merged_at.is_some() {
        let owner = crate::claims::owner_id();
        let marked = mcp
            .with_backend_db(|conn| agentflare_backend::item::mark_completed(conn, item_id, &owner))
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false);
        if !marked {
            let msg = format!(
                "duplicate work: PR #{} already merged for {item_id}, but mark_completed \
                 didn't confirm success -- leaving claim armed for retry",
                pr.number
            );
            let _ = writeln!(log, "{msg}");
            crate::ui::error(&msg);
            return 1.into();
        }
        crate::worktree::cleanup_worktree(item, worktree_path);
        crate::worktree::relabel_pr_completed(item, worktree_path);
        let _ = writeln!(
            log,
            "duplicate work: PR #{} already merged for {item_id} -- auto-completed",
            pr.number
        );
        format!(
            "## agentflare work — duplicate work detected\n\nPR #{} ({}) already merged for \
             this item; auto-completing instead of redispatching.",
            pr.number, pr.html_url
        )
    } else {
        let _ = writeln!(
            log,
            "duplicate work: open PR #{} already covers {item_id} -- skipping dispatch",
            pr.number
        );
        format!(
            "## agentflare work — needs human review\n\nFound an open PR #{} ({}) already \
             covering this item; skipping dispatch to avoid opening a duplicate.",
            pr.number, pr.html_url
        )
    };
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item_id.into()),
        body: Some(body.clone()),
        ..Default::default()
    });
    match mcp.item_release(ItemRequest {
        action: "release".into(),
        id: Some(item_id.into()),
        ..Default::default()
    }) {
        Ok(_) => claim_guard.disarm(),
        Err(e) => {
            let _ = writeln!(
                log,
                "duplicate work: item_release failed for {item_id}: {e} -- \
                 leaving claim armed so ClaimGuard's Drop retries it"
            );
        }
    }
    if let Some(recipient) = notify_recipient {
        notify(recipient, &body, item_id);
    }
    0.into()
}

/// Thin wrapper so `execute_work_impl`'s call site is a one-line short
/// circuit instead of repeating `find_duplicate_pr`/`handle_duplicate_pr`'s
/// full argument lists inline.
fn duplicate_pr_short_circuit(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    worktree_path: &std::path::Path,
    notify_recipient: Option<&str>,
    claim_guard: &mut ClaimGuard,
    log: &mut dyn std::io::Write,
) -> Option<WorkOutcome> {
    let pr = find_duplicate_pr(item, worktree_path)?;
    Some(handle_duplicate_pr(
        mcp,
        &item.id,
        item,
        worktree_path,
        &pr,
        notify_recipient,
        claim_guard,
        log,
    ))
}
