/// A still-open, CI-red duplicate stops blocking redispatch once it's sat
/// unmerged this long (item #192: item #186/PR #597 sat CI-red across at
/// least 4 blocked attempts over 2 days, including the daemon's own CI
/// self-repair trigger, with no route back to re-examination). Chosen to
/// comfortably outlast a transient CI blip or an in-flight self-repair
/// attempt while still surfacing a genuinely stuck PR within a few days
/// instead of indefinitely.
const STALE_DUPLICATE_DAYS: i64 = 3;

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
/// so `handle_duplicate_pr` reports the stronger evidence, on the rare
/// chance both exist.
fn find_duplicate_pr(
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
) -> Option<crate::github::models::PullRequest> {
    let repo = crate::github::RepoId::resolve_from_remote(repo_root)?;
    let client = crate::github::Client::new().ok()?;
    let prs =
        crate::github::pulls::find_by_item_marker(&client, &repo, item.sequence_id, &item.id)
            .ok()?;
    let pr = pick_duplicate_pr(
        prs,
        flare_git_core::branch::current_branch(repo_root).as_deref(),
        crate::worktree::pr_number_from_metadata(item),
    )?;
    if pr.merged_at.is_none() {
        let ci_failing = matches!(
            crate::worktree::pr_ci_status(item, repo_root),
            crate::worktree::PrCiStatus::Failing { .. }
        );
        if is_stale_ci_red(&pr, ci_failing, chrono::Utc::now()) {
            return None;
        }
    }
    Some(pr)
}

/// Whether `pr` -- an open duplicate that survived `pick_duplicate_pr`'s
/// exclusions -- is both CI-red and older than `STALE_DUPLICATE_DAYS`: the
/// minimum content signal available without an LLM judge that a PR "already
/// covering this item" no longer actually does (item #192). A fresh PR, one
/// still running checks, or one with passing/unknown CI keeps blocking, since
/// only a PR that's been failing for a while is a plausible dead end rather
/// than routine in-progress work. `ci_failing` is passed in (rather than
/// computed here from `item`/`repo_root`) so this decision is unit-testable
/// without a mock GitHub server, matching `pick_duplicate_pr`'s split.
fn is_stale_ci_red(
    pr: &crate::github::models::PullRequest,
    ci_failing: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    ci_failing
        && pr
            .created_at
            .as_deref()
            .and_then(|c| chrono::DateTime::parse_from_rfc3339(c).ok())
            .is_some_and(|created| {
                now.signed_duration_since(created) > chrono::Duration::days(STALE_DUPLICATE_DAYS)
            })
}

/// Picks which match to act on when `find_by_item_marker` returns more than
/// one PR for the same item — a merged one wins as the stronger evidence
/// `handle_duplicate_pr` reports for human confirmation. A closed-but-unmerged
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
/// ever pick back up (reproduced live on item #186/PR #597).
///
/// `own_pr_number` (`metadata.pr.number`, item #192) is the same exclusion
/// by a more durable signal than the current branch: the branch check only
/// fires once the worktree is actually checked out onto that exact branch,
/// which self-repair's own dispatch didn't reliably guarantee -- item
/// #186/#597 kept getting blocked by this same guard across at least 4
/// attempts *after* the branch fix landed, including the daemon's own CI
/// self-repair trigger. A PR `push_and_open_pr` already recorded as this
/// item's own is never a competing duplicate, regardless of what's checked
/// out right now.
///
/// A *merged* match still always short-circuits regardless of branch or
/// ownership, since that's this function's other job: surface an item whose
/// PR landed while its tracked state fell out of sync (items #122/#156) for a
/// human to confirm.
fn pick_duplicate_pr(
    mut prs: Vec<crate::github::models::PullRequest>,
    current_branch: Option<&str>,
    own_pr_number: Option<u64>,
) -> Option<crate::github::models::PullRequest> {
    prs.retain(|pr| pr.merged_at.is_some() || pr.state == "open");
    prs.retain(|pr| {
        pr.merged_at.is_some()
            || (own_pr_number != Some(pr.number)
                && current_branch
                    .zip(pr.head.as_ref())
                    .is_none_or(|(current, head)| head.git_ref != current))
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
/// Both branches skip dispatch and flag for a human; neither touches the
/// item's tracker state. A merged match used to self-heal (mark completed,
/// clean up the worktree, relabel the PR) on the strength of nothing but a
/// text marker in the PR body -- item #595 showed that marker can name the
/// wrong item (PR #636 was another item's work, stamped "for item #198"),
/// which completed a never-implemented item with zero work done. Completing
/// it (or not) is now the human's call, and nothing here is irreversible.
///
/// `claim_guard` is only disarmed once `item_release` actually confirms
/// success — a failed release leaves it armed so `Drop`'s best-effort retry
/// is still the backstop, matching `release_and_comment`'s own contract.
fn handle_duplicate_pr(
    mcp: &AgentflareMcp,
    item_id: &str,
    item: &agentflare_backend::item::Item,
    pr: &crate::github::models::PullRequest,
    notify_recipient: Option<&str>,
    claim_guard: &mut ClaimGuard,
    log: &mut dyn std::io::Write,
) -> WorkOutcome {
    let body = if pr.merged_at.is_some() {
        let _ = writeln!(
            log,
            "duplicate work: merged PR #{} carries {item_id}'s marker -- needs human \
             confirmation, skipping dispatch",
            pr.number
        );
        format!(
            "## agentflare work — needs human review\n\nPR #{} ({}) is already merged and \
             carries this item's `for item #{}` marker, so dispatch was skipped. The marker \
             alone can't prove the PR covers this item's scope; confirm it does, then complete \
             the item by hand.",
            pr.number, pr.html_url, item.sequence_id
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
    let comment = mcp.comment_impl(CommentRequest {
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
    // The comment is the persistent human-review signal: with the claim now
    // released and nothing else recorded, reporting success on a failed
    // comment would drop the duplicate silently. Fail so the job retries.
    match comment {
        Ok(_) => 0.into(),
        Err(e) => {
            let _ = writeln!(
                log,
                "duplicate work: could not record the human-review comment on {item_id}: {e}"
            );
            1.into()
        }
    }
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
        &pr,
        notify_recipient,
        claim_guard,
        log,
    ))
}
