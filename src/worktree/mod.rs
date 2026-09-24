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
    let branch = flare_git_core::worktree::resolve_item_task_branch(item, repo_root);
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

pub use flare_git_core::worktree::{RebaseOutcome, rebase_item_worktree};

/// Commits any uncommitted changes in `item`'s worktree. See
/// `flare_git_core::worktree::commit_uncommitted`.
pub fn commit_uncommitted(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    message: &str,
) -> CommitOutcome {
    flare_git_core::worktree::commit_uncommitted(item, repo_root, message)
}

/// See `flare_git_core::worktree::commit_uncommitted_at`.
pub fn commit_uncommitted_at(
    worktree_path: &Path,
    message: &str,
    no_verify: bool,
) -> CommitOutcome {
    flare_git_core::worktree::commit_uncommitted_at(worktree_path, message, no_verify)
}

/// See `flare_git_core::worktree::head_sha`.
pub fn head_sha(worktree_path: &Path) -> Option<String> {
    flare_git_core::worktree::head_sha(worktree_path)
}

/// See `flare_git_core::worktree::squash_since`.
pub fn squash_since(worktree_path: &Path, base_sha: &str) -> Result<(), String> {
    flare_git_core::worktree::squash_since(worktree_path, base_sha)
}

/// The stored `metadata.pr.number` a prior `push_and_open_pr` call left on
/// the item -- the authoritative PR identity once set, since it was read
/// straight off GitHub's response at PR-creation time instead of
/// reconstructed from the branch name afterward. `None` for items that
/// predate this field (opened before this fix, or never pushed through
/// `push_and_open_pr`).
pub(crate) fn pr_number_from_metadata(item: &agentflare_backend::item::Item) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(&item.metadata)
        .ok()?
        .get("pr")?
        .get("number")?
        .as_u64()
}

mod discovery;
pub(crate) use discovery::{discover_untracked_prs, tracked_pr_numbers};
mod draft;
pub(crate) use draft::{mark_pr_ready, pr_marked_ready};

/// Checks whether `item`'s branch already has a merged PR — the promotion
/// signal `check_merge` uses to move an item out of "in_review" (item
/// #420). Soft-fails like `push_and_open_pr`: no GitHub credentials, no
/// resolvable remote, or a lookup failure all just report "not merged yet"
/// rather than erroring, since the caller's fallback is simply to check
/// again later.
///
/// Prefers `metadata.pr.number` (set by `push_and_open_pr` the moment the
/// PR was actually created or found) when present, since a direct
/// `pulls::get` by number needs no branch reconstruction at all. Only items
/// that predate this field fall back to the old heuristic: `find_existing`
/// matches on branch name alone, and branch names get reused across items
/// over time, so a match is only trusted as this item's own PR when
/// `marks_item` confirms it -- otherwise an unrelated, already-merged PR
/// from a past item would fool `check_merge` into promoting this item off
/// someone else's merge (item #63). That branch reconstruction can also
/// simply be wrong: `resolve_item_task_branch` rebuilds it from whatever's
/// checked out on disk (or a freshly recomputed slug once the worktree is
/// gone), which can drift from the branch the PR was actually opened
/// against (item #191).
pub fn is_pr_merged(item: &agentflare_backend::item::Item, repo_root: &Path) -> bool {
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return false;
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(_) => return false,
    };
    is_pr_merged_impl(item, repo_root, &client, &repo)
}

fn is_pr_merged_impl(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    client: &crate::github::Client,
    repo: &RepoId,
) -> bool {
    if let Some(number) = pr_number_from_metadata(item) {
        return match crate::github::pulls::get(client, repo, number) {
            Ok(pr) => pr.merged_at.is_some(),
            Err(e) => {
                eprintln!(
                    "worktree: could not check merge status for item {}: {e}",
                    item.id
                );
                false
            }
        };
    }
    let branch = flare_git_core::worktree::resolve_item_task_branch(item, repo_root);
    match crate::github::pulls::find_existing(client, repo, &branch) {
        Ok(Some(pr)) => {
            pr.merged_at.is_some()
                && crate::github::pulls::marks_this_item(
                    pr.body.as_deref(),
                    item.sequence_id,
                    &item.id,
                )
        }
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
///
/// Same branch-reuse hazard as `is_pr_merged`: a `find_existing` match is
/// only relabeled once `marks_item` confirms it's this item's own PR, so an
/// unrelated PR that happens to share the branch name never gets its
/// labels touched on this item's behalf (item #63).
///
/// Prefers `metadata.pr.number` when present, same as `is_pr_merged`: the
/// recorded number needs no branch reconstruction or PR search at all.
pub fn relabel_pr_completed(item: &agentflare_backend::item::Item, repo_root: &Path) {
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return;
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(_) => return,
    };
    relabel_pr_completed_impl(item, repo_root, &client, &repo);
}

fn relabel_pr_completed_impl(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    client: &crate::github::Client,
    repo: &RepoId,
) {
    let number = match pr_number_from_metadata(item) {
        Some(number) => number,
        None => match find_own_pr_by_branch(item, repo_root, client, repo) {
            Some(number) => number,
            None => return,
        },
    };
    if let Err(e) =
        crate::github::issues::remove_label(client, repo, number, "agentflare:in-review")
    {
        eprintln!("worktree: could not remove agentflare:in-review from PR #{number}: {e}");
    }
    if let Err(e) = crate::github::issues::add_labels(
        client,
        repo,
        number,
        &["agentflare:completed".to_string()],
    ) {
        eprintln!("worktree: could not add agentflare:completed to PR #{number}: {e}");
    }
}

/// The branch-heuristic fallback for items with no recorded
/// `metadata.pr.number`: a `find_existing` match, trusted only once
/// `marks_this_item` confirms it is this item's own PR.
fn find_own_pr_by_branch(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    client: &crate::github::Client,
    repo: &RepoId,
) -> Option<u64> {
    let branch = flare_git_core::worktree::resolve_item_task_branch(item, repo_root);
    let pr = match crate::github::pulls::find_existing(client, repo, &branch) {
        Ok(Some(pr))
            if crate::github::pulls::marks_this_item(
                pr.body.as_deref(),
                item.sequence_id,
                &item.id,
            ) =>
        {
            pr
        }
        Ok(Some(_)) | Ok(None) => return None,
        Err(e) => {
            eprintln!(
                "worktree: could not look up PR to relabel for item {}: {e}",
                item.id
            );
            return None;
        }
    };
    Some(pr.number)
}

/// CI signal the in-review sweep (`supervisor::run_review_sweep`, item #65)
/// polls per item: merged (promote), failing (self-repair), CI-green with a
/// human approval label attached (auto-merge, item #194), cleanly behind the
/// base branch with no conflict (update-branch, item #197's follow-up), or
/// nothing actionable yet. `Unknown` covers every soft-fail case
/// `is_pr_merged` above also treats as "not merged yet" -- no credentials,
/// no resolvable remote, no PR found, or a lookup error -- since the
/// caller's fallback is simply to poll again next tick.
/// What GitHub's native auto-merge needs beyond a PR number: the PR's
/// GraphQL node id (the `enablePullRequestAutoMerge` mutation's
/// `pullRequestId`) and whether auto-merge is already armed on it, so the
/// sweep neither re-arms it every tick nor arms it blind. Both come from the
/// same fetch as the CI verdict (GraphQL `id`/`autoMergeRequest`, REST
/// `node_id`/`auto_merge`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutoMergeRef {
    pub node_id: Option<String>,
    pub enabled: bool,
}

#[derive(Debug)]
pub enum PrCiStatus {
    Merged,
    /// CI has a failed check. Carries the PR number so `run_review_sweep`
    /// can post a GitHub-visible stage label/comment for the self-repair
    /// dispatch without a second API round-trip to look the number back up.
    /// Also carries the PR's current GitHub labels (already in hand from the
    /// same fetch as `checks`), so a self-repair dispatch can tell whether
    /// it's replacing a plain in-review label or a stale
    /// `agentflare:review-repair` one left over from item #273's CodeRabbit
    /// repair path -- without it, self-repair only ever cleared the
    /// in-review label, leaving the review-repair label stacked on top.
    Failing {
        number: u64,
        checks: Vec<String>,
        labels: Vec<String>,
    },
    Pending,
    /// CI is green. Carries the PR number and its GitHub label names so
    /// `run_review_sweep` can decide whether to auto-merge without a second
    /// API round-trip just to re-fetch labels, plus the head commit the
    /// checks were judged against (`None` only if GitHub didn't report one)
    /// so that merge is pinned to exactly that commit.
    Passing {
        number: u64,
        labels: Vec<String>,
        head_sha: Option<String>,
        auto_merge: AutoMergeRef,
    },
    /// Required CI is green but GitHub's `mergeStateStatus` is `BLOCKED` on
    /// review: branch protection wants an approving review
    /// (`reviewDecision == REVIEW_REQUIRED`) or a reviewer requested changes
    /// (`CHANGES_REQUESTED`). Distinct from `Passing` because no merge can
    /// succeed until a human acts -- the sweep surfaces the approval gate
    /// instead of attempting one -- and from `Pending` because nothing
    /// automated is still running that would ever move it.
    AwaitingReview {
        number: u64,
        labels: Vec<String>,
        /// `reviewDecision == CHANGES_REQUESTED` (a reviewer asked for
        /// changes) as opposed to `REVIEW_REQUIRED` (nobody has approved
        /// yet) -- the approval card tells the human which it is.
        changes_requested: bool,
        /// The head the green verdict was made on, and the auto-merge
        /// handle: with the approval label attached and no findings, the
        /// sweep arms GitHub's auto-merge here so the review landing is
        /// all it takes to merge.
        head_sha: Option<String>,
        auto_merge: AutoMergeRef,
    },
    /// GitHub's own `mergeable_state == "behind"` -- mergeable, no conflict,
    /// just missing commits the base branch has gained since this PR was
    /// opened/last updated. Checked before CI status is even fetched: a
    /// stale-but-behind PR's existing check runs are stale too, and re-fetching
    /// them here would be wasted work the branch update is about to
    /// invalidate anyway.
    Behind {
        number: u64,
        /// The head the "behind" verdict was made against, sent as
        /// update-branch's `expected_head_sha`.
        head_sha: Option<String>,
    },
    /// GitHub's own `mergeable_state == "dirty"` -- unlike `Behind` this is a
    /// real conflict, not a clean fast-forward, so `run_review_sweep` can't
    /// resolve it with the same no-judgment server-side call
    /// `update_stale_branch` uses for `Behind`. Checked in the same place as
    /// `Behind` (before CI status is fetched) for the same reason: a
    /// conflicted PR's existing check runs ran against a merge base that's
    /// about to be invalidated regardless of what they say.
    Conflicting {
        number: u64,
    },
    /// The PR was closed without being merged. Nothing on that PR will ever
    /// land, so `run_review_sweep` sends the item back for a fresh attempt
    /// instead of polling a dead PR (or self-repairing it) forever.
    Closed {
        number: u64,
    },
    /// Still a draft. `push_and_open_pr` opens every PR as one and
    /// `item_done` marks it ready once the item is in review, so a draft on
    /// an in-review item means that flip never landed (`run_review_sweep`
    /// retries it, see `draft::mark_pr_ready`) -- or a human converted it
    /// back on purpose, which the sweep respects. Checked before CI state:
    /// a draft can neither merge nor be handed to reviewers, so its checks
    /// are not yet actionable either way.
    Draft {
        number: u64,
        /// The GraphQL node id the ready-for-review mutation needs, when the
        /// fetch had it.
        node_id: Option<String>,
    },
    Unknown,
}

/// Same "total>0 && not pending" gate `cli::git::wait_for_checks` polls on,
/// applied once instead of in a loop -- the sweep itself provides the retry
/// cadence across ticks.
///
/// Same metadata-first / branch-heuristic-fallback contract as
/// `is_pr_merged`: `metadata.pr.number` (when present) is fetched directly
/// via `pulls::get`; otherwise a `find_existing` match is only trusted once
/// `marks_item` confirms it's this item's own PR, so an unrelated PR
/// sharing the branch name can't report its CI status as this item's (item
/// #63).
pub fn pr_ci_status(item: &agentflare_backend::item::Item, repo_root: &Path) -> PrCiStatus {
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return PrCiStatus::Unknown;
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(_) => return PrCiStatus::Unknown,
    };
    pr_ci_status_impl(item, repo_root, &client, &repo)
}

fn pr_ci_status_impl(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    client: &crate::github::Client,
    repo: &RepoId,
) -> PrCiStatus {
    let pr = match pr_number_from_metadata(item) {
        Some(number) => match crate::github::pulls::get(client, repo, number) {
            Ok(pr) => pr,
            Err(e) => {
                eprintln!(
                    "worktree: could not check PR status for item {}: {e}",
                    item.id
                );
                return PrCiStatus::Unknown;
            }
        },
        None => {
            let branch = flare_git_core::worktree::resolve_item_task_branch(item, repo_root);
            match crate::github::pulls::find_existing(client, repo, &branch) {
                Ok(Some(pr))
                    if crate::github::pulls::marks_this_item(
                        pr.body.as_deref(),
                        item.sequence_id,
                        &item.id,
                    ) =>
                {
                    pr
                }
                Ok(Some(_)) | Ok(None) => return PrCiStatus::Unknown,
                Err(e) => {
                    eprintln!(
                        "worktree: could not check PR status for item {}: {e}",
                        item.id
                    );
                    return PrCiStatus::Unknown;
                }
            }
        }
    };
    if pr.merged_at.is_some() {
        return PrCiStatus::Merged;
    }
    if pr.state == "closed" {
        return PrCiStatus::Closed { number: pr.number };
    }
    if pr.draft {
        return PrCiStatus::Draft {
            number: pr.number,
            node_id: pr.node_id.clone(),
        };
    }
    let head_sha = pr
        .head
        .as_ref()
        .map(|h| h.sha.clone())
        .filter(|s| !s.is_empty());
    if pr.mergeable == Some(true) && pr.mergeable_state.as_deref() == Some("behind") {
        return PrCiStatus::Behind {
            number: pr.number,
            head_sha,
        };
    }
    if pr.mergeable == Some(false) && pr.mergeable_state.as_deref() == Some("dirty") {
        return PrCiStatus::Conflicting { number: pr.number };
    }
    let Some(sha) = head_sha else {
        return PrCiStatus::Unknown;
    };
    // Both CI APIs: the Checks API (Actions and most apps) and the older
    // Statuses API (third-party CI, CLA bots) -- branch protection can
    // require contexts from either. REST can't say which are required, so
    // every context counts on this path.
    let checks =
        crate::github::actions::list_check_runs(client, repo, &sha).and_then(|mut runs| {
            runs.extend(crate::github::actions::list_commit_statuses(
                client, repo, &sha,
            )?);
            Ok(runs)
        });
    let checks = match checks {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "worktree: could not fetch check status for item {}: {e}",
                item.id
            );
            return PrCiStatus::Unknown;
        }
    };
    decide_from_checks(
        pr.number,
        &checks,
        pr.labels.into_iter().map(|l| l.name).collect(),
        &MergeSignals {
            mergeable: pr.mergeable,
            mergeable_state: pr.mergeable_state.as_deref(),
            // REST's PR object has no review decision; a review-blocked PR
            // stays `Pending` on this path.
            review_decision: None,
            rollup_state: None,
            head_sha: Some(&sha),
            auto_merge: AutoMergeRef {
                node_id: pr.node_id.clone(),
                enabled: pr.auto_merge.is_some(),
            },
            // REST doesn't say; a merge-queue repo's PR stays `Pending`
            // on this path, as it did before.
            merge_queue_enabled: false,
            in_merge_queue: false,
        },
    )
}

/// GitHub's own view of a PR's mergeability, alongside its checks -- what
/// `decide_from_checks` needs beyond the check list itself.
struct MergeSignals<'a> {
    mergeable: Option<bool>,
    /// Lower-case, REST's `mergeable_state` vocabulary.
    mergeable_state: Option<&'a str>,
    /// Upper-case GraphQL `reviewDecision`; always `None` on the REST path.
    review_decision: Option<&'a str>,
    /// Upper-case GraphQL `statusCheckRollup.state`; always `None` on REST.
    rollup_state: Option<&'a str>,
    head_sha: Option<&'a str>,
    auto_merge: AutoMergeRef,
    /// The base branch merges through a merge queue (GraphQL only).
    merge_queue_enabled: bool,
    /// The PR is already in that queue (GraphQL only).
    in_merge_queue: bool,
}

/// The part of the CI-status decision tree that only needs check-run data
/// (merged/behind are decided from the PR itself before this is reached) --
/// shared verbatim by `pr_ci_status_impl`'s per-PR REST fetch and
/// `pr_ci_status_from_batch`'s GraphQL-batch fetch, so the two fetch paths
/// can never quietly disagree on what a given set of check runs means.
///
/// Which contexts count: when branch protection marks any context required
/// (GraphQL's `isRequired`), only the required ones decide -- an optional
/// flaky job must neither block nor trigger self-repair on a PR GitHub would
/// happily merge. When none is marked (no protection, or the REST path,
/// which can't tell), every context counts, as before.
///
/// A PR with no contexts at all is not left `Pending` forever: a repo with
/// no CI whose PR GitHub reports cleanly mergeable (`mergeable_state ==
/// "clean"`, so no required context is outstanding either) is `Passing`.
/// The window right after a push, before CI has created its first check
/// run, can also look like that on a repo whose checks are all optional;
/// merges stay behind the human approval label and are pinned to the head
/// SHA, so at worst the approval-gate card arrives a tick early.
fn decide_from_checks(
    number: u64,
    checks: &[crate::github::models::CheckRun],
    labels: Vec<String>,
    signals: &MergeSignals<'_>,
) -> PrCiStatus {
    let passing = |labels: Vec<String>| PrCiStatus::Passing {
        number,
        labels,
        head_sha: signals.head_sha.map(str::to_string),
        auto_merge: signals.auto_merge.clone(),
    };
    let awaiting_review = signals.mergeable_state == Some("blocked")
        && matches!(
            signals.review_decision,
            Some("REVIEW_REQUIRED") | Some("CHANGES_REQUESTED")
        );
    let awaiting = |labels: Vec<String>| PrCiStatus::AwaitingReview {
        number,
        labels,
        changes_requested: signals.review_decision == Some("CHANGES_REQUESTED"),
        head_sha: signals.head_sha.map(str::to_string),
        auto_merge: signals.auto_merge.clone(),
    };
    // Enqueued: the merge queue's own CI run on the merge group decides
    // now, and GitHub merges (or kicks it back out) by itself.
    if signals.in_merge_queue {
        return PrCiStatus::Pending;
    }
    let relevant: Vec<crate::github::models::CheckRun> = if checks.iter().any(|c| c.required) {
        checks.iter().filter(|c| c.required).cloned().collect()
    } else {
        checks.to_vec()
    };
    if relevant.is_empty() {
        // No context came back; GitHub's own roll-up is the next-best signal.
        match signals.rollup_state {
            Some("FAILURE") | Some("ERROR") => {
                return PrCiStatus::Failing {
                    number,
                    checks: vec!["statusCheckRollup".to_string()],
                    labels,
                };
            }
            Some("PENDING") | Some("EXPECTED") => return PrCiStatus::Pending,
            _ => {}
        }
        if signals.mergeable == Some(true) {
            if signals.mergeable_state == Some("clean") {
                return passing(labels);
            }
            if awaiting_review {
                return awaiting(labels);
            }
        }
        return PrCiStatus::Pending;
    }
    let summary = crate::github::mcp::checks_wait_summary(&relevant, 0);
    if summary["pending"].as_bool().unwrap_or(true) {
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
    if !failed.is_empty() {
        return PrCiStatus::Failing {
            number,
            checks: failed,
            labels,
        };
    }
    // Every required context is green, and GitHub still says "blocked"
    // purely on review: hand it to the approval gate rather than polling a
    // PR that only a human can unblock.
    if awaiting_review {
        return awaiting(labels);
    }
    // The check-run list above only reflects what GitHub has created so far --
    // gated jobs (e.g. a `build` matrix behind a `changes` job) may not exist
    // yet even though every check-run seen so far is green, which would
    // otherwise read as "0 pending, 0 failed" and report Passing well before
    // the PR is actually mergeable. GitHub's own `mergeable_state` already
    // accounts for the full required-checks list, so "blocked" here means
    // more is still outstanding -- treat it as still-pending rather than
    // trusting the incomplete snapshot. Item #587: the original fix only
    // special-cased "blocked" and the ping still fired early, because right
    // after a push GitHub reports "unknown" (it hasn't finished computing
    // mergeability at all yet) before it ever settles into "blocked" --
    // that's the exact same incomplete-snapshot window, just caught one tick
    // earlier, so it gets the same treatment.
    if matches!(signals.mergeable_state, Some("blocked") | Some("unknown")) {
        // A base branch with a merge queue never reports "clean": a direct
        // merge is refused there by design, and the PR reads "blocked"
        // until it goes through the queue. With every required context
        // green and no review outstanding, that is the queue's cue -- the
        // sweep's merge path arms auto-merge, which is how a PR enters the
        // queue, and the queue's own CI run on the merge group is the real
        // gate for whatever a still-missing gated job would have covered.
        if signals.merge_queue_enabled && signals.mergeable_state == Some("blocked") {
            return passing(labels);
        }
        return PrCiStatus::Pending;
    }
    passing(labels)
}

/// `pr_ci_status_impl`'s decision tree applied to data already fetched in
/// bulk by `github::graphql::batch_pr_status` instead of one REST call per
/// PR -- see that module's doc comment for why. Deliberately reuses
/// `decide_from_checks` for the post-merged/behind part of the tree instead
/// of re-deriving it, so `run_review_sweep`'s batched fetch and
/// `pr_ci_status`'s single-item fetch (still used by
/// `cli::work_duplicate_pr`) can never diverge on what the same check-run
/// data means.
pub(crate) fn pr_ci_status_from_batch(
    number: u64,
    data: &crate::github::graphql::BatchPrData,
) -> PrCiStatus {
    if data.merged {
        return PrCiStatus::Merged;
    }
    if data.closed {
        return PrCiStatus::Closed { number };
    }
    if data.is_draft {
        return PrCiStatus::Draft {
            number,
            node_id: data.node_id.clone(),
        };
    }
    if data.mergeable == Some(true) && data.mergeable_state.as_deref() == Some("behind") {
        return PrCiStatus::Behind {
            number,
            head_sha: data.head_sha.clone(),
        };
    }
    if data.mergeable == Some(false) && data.mergeable_state.as_deref() == Some("dirty") {
        return PrCiStatus::Conflicting { number };
    }
    decide_from_checks(
        number,
        &data.checks,
        data.labels.clone(),
        &MergeSignals {
            mergeable: data.mergeable,
            mergeable_state: data.mergeable_state.as_deref(),
            review_decision: data.review_decision.as_deref(),
            rollup_state: data.rollup_state.as_deref(),
            head_sha: data.head_sha.as_deref(),
            auto_merge: AutoMergeRef {
                node_id: data.node_id.clone(),
                enabled: data.auto_merge_enabled,
            },
            merge_queue_enabled: data.merge_queue_enabled,
            in_merge_queue: data.in_merge_queue,
        },
    )
}

/// Brings a cleanly-behind PR's branch up to date with the base branch via
/// GitHub's own server-side "Update branch" operation -- only ever called
/// from `run_review_sweep`'s `Behind` arm, so `mergeable_state == "behind"`
/// is structurally already confirmed by the time this runs. Logs and
/// returns `false` on failure (a concurrent push moving the branch head, a
/// transient API error) rather than retrying in-line -- same "let the next
/// sweep tick see the real current state and decide again" shape
/// `merge_approved_pr` already uses for its own GitHub call.
pub fn update_stale_branch(repo_root: &Path, number: u64, head_sha: Option<&str>) -> bool {
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return false;
    };
    let Ok(client) = crate::github::Client::new() else {
        return false;
    };
    update_branch_pr(&client, &repo, number, head_sha)
}

/// The actual GitHub update-branch call. Split out from `update_stale_branch`
/// so tests can drive it against a mock server instead of `Client::new()`'s
/// real credentials/host, mirroring `supervisor::merge_approved_pr`'s own
/// test-seam split. Also the idempotency boundary a batched, snapshot-driven
/// `run_review_sweep` now depends on: the batch's `Behind` verdict can be
/// stale by the time this runs (CI/base-branch state moved on, or another
/// sweep/daemon already updated it), so a rejection here -- GitHub itself
/// refusing an already-current or already-merged branch -- must fall through
/// to "log and skip", not panic or retry in-line, exactly like a duplicate
/// `merge` call already does. `head_sha` pins the update to the head the
/// verdict was made on: a branch someone pushed to since is left alone and
/// re-judged next tick.
fn update_branch_pr(
    client: &crate::github::Client,
    repo: &RepoId,
    number: u64,
    head_sha: Option<&str>,
) -> bool {
    match crate::github::pulls::update_branch(client, repo, number, head_sha) {
        Ok(()) => true,
        Err(e) if crate::github::pulls::is_head_moved(&e) => {
            eprintln!(
                "worktree: PR #{number} in {repo} got new commits since it was checked; \
                 leaving update-branch to the next sweep"
            );
            false
        }
        Err(e) => {
            eprintln!("worktree: update-branch failed for PR #{number} in {repo}: {e}");
            false
        }
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

/// `Closes #N` for an item the GitHub bridge adopted from issue `N` (it
/// records the link as `external_source = "github"`, `external_id = N`), so
/// merging the PR closes the issue it was opened for. `issue_repo` is the
/// repo the bridge watches; when it differs from `pr_repo` the reference
/// is spelled `owner/repo#N`, which GitHub also honors across repos. `None`
/// for any item that didn't come from an issue.
fn closes_issue_line(
    item: &agentflare_backend::item::Item,
    issue_repo: Option<&RepoId>,
    pr_repo: &RepoId,
) -> Option<String> {
    if item.external_source.as_deref() != Some(crate::github::bridge::items::EXTERNAL_SOURCE) {
        return None;
    }
    let number: u64 = item.external_id.as_deref()?.trim().parse().ok()?;
    Some(match issue_repo {
        Some(r) if r != pr_repo => format!("Closes {r}#{number}"),
        _ => format!("Closes #{number}"),
    })
}

/// Human-readable attribution appended to the PR body -- who opened it and
/// on which machine, so a reviewer never has to guess whether a PR came
/// from agentflare. Unlike `bridge::marker::Marker`, this is not a
/// parseable format: nothing reads a PR footer back (the bridge doesn't
/// poll PRs for claim state the way it polls issues), so there is no
/// format to keep stable.
fn pr_footer(agent: &str, machine: &str, sequence_id: i64, item_id: &str) -> String {
    format!(
        "---\n_Opened by `{agent}` on **{machine}** for item #{sequence_id} via agentflare._\n{}",
        crate::github::pulls::item_id_tag(item_id)
    )
}

/// Conventional-commit types accepted by `.github/workflows/pr-title.yml`'s
/// `amannn/action-semantic-pull-request` check. Mirrors that file's `types`
/// list and `cliff.toml`'s `commit_parsers` -- keep all three in sync.
const CONVENTIONAL_TYPES: &[&str] = &[
    "feat", "fix", "docs", "perf", "refactor", "style", "test", "chore", "ci",
];

/// Maps a common non-conventional prefix word (e.g. "Bugfix", "Feature") to
/// the conventional type it most likely means.
fn infer_type_from_word(word: &str) -> Option<&'static str> {
    match word {
        "bug" | "bugfix" | "hotfix" | "patch" => Some("fix"),
        "feature" | "features" => Some("feat"),
        "doc" | "documentation" => Some("docs"),
        "performance" | "optimization" | "optimisation" => Some("perf"),
        "refactoring" => Some("refactor"),
        "styling" | "formatting" | "lint" | "linting" => Some("style"),
        "testing" | "tests" => Some("test"),
        "cleanup" | "chores" | "maintenance" | "misc" => Some("chore"),
        "pipeline" | "workflow" | "workflows" => Some("ci"),
        _ => None,
    }
}

/// Falls back to scanning the full (lowercased) item name for a keyword when
/// no prefix word gave a match. Defaults to `chore` when nothing matches.
fn infer_type_from_text(lower_text: &str) -> &'static str {
    if lower_text.contains("bug") || lower_text.contains("fix") {
        "fix"
    } else if lower_text.contains("feature") || lower_text.contains("implement") {
        "feat"
    } else if lower_text.contains("doc") {
        "docs"
    } else if lower_text.contains("perf") || lower_text.contains("optimiz") {
        "perf"
    } else if lower_text.contains("refactor") {
        "refactor"
    } else if lower_text.contains("style") || lower_text.contains("lint") {
        "style"
    } else if lower_text.contains("test") {
        "test"
    } else if lower_text.contains("pipeline") || lower_text.contains("workflow") {
        "ci"
    } else {
        "chore"
    }
}

/// Derives a PR title that satisfies `pr-title.yml`'s conventional-commit
/// check from a raw item name, which is free-form text ("Bugfix: ...",
/// "Feature: ...", plain English) with no guaranteed relationship to
/// `CONVENTIONAL_TYPES`. If `name` already starts with `type: ` or
/// `type(scope): `, that prefix is lowercased and passed through unchanged;
/// otherwise a type is inferred from the leading word (if any) or, failing
/// that, from keywords anywhere in `name`, and prepended -- `name` itself is
/// never dropped, only ever prefixed.
fn conventional_pr_title(name: &str) -> String {
    let trimmed = name.trim();
    if let Some((head, rest)) = trimmed.split_once(':') {
        let rest = rest.trim();
        if !rest.is_empty() {
            let head = head.trim();
            let type_token = head.split('(').next().unwrap_or(head).trim();
            let scope_suffix = &head[type_token.len()..];
            let lower = type_token.to_lowercase();
            if CONVENTIONAL_TYPES.contains(&lower.as_str()) {
                return format!("{lower}{scope_suffix}: {rest}");
            }
            if let Some(mapped) = infer_type_from_word(&lower) {
                return format!("{mapped}: {rest}");
            }
        }
    }
    let inferred = infer_type_from_text(&trimmed.to_lowercase());
    format!("{inferred}: {trimmed}")
}

/// Merges `{"pr": {"number": N, "branch": "..."}}` into `item`'s stored
/// metadata (without clobbering unrelated keys like `size`/`workflow_run_id`)
/// and persists it -- the identity `is_pr_merged`/`pr_ci_status` read back
/// directly instead of reconstructing the branch name to rediscover the
/// same PR (item #191: that reconstruction drifted from the PR's real
/// branch, and `check_merge` reported "not merged yet" for a PR that had
/// actually merged).
///
/// Re-reads the row's *current* metadata and writes it back inside one
/// IMMEDIATE transaction rather than merging into `item.metadata`: `item`
/// is a snapshot taken before a push that can run for up to two minutes,
/// and writing that stale blob back whole used to silently undo any
/// metadata another writer (the pipeline's `workflow_run_id`, a repair
/// tracker) stored in the meantime. Non-object metadata is coerced to an
/// empty object first -- `Value` indexing panics assigning into anything
/// that isn't already `Object`.
fn merge_and_persist_pr_identity(
    conn: &rusqlite::Connection,
    item: &agentflare_backend::item::Item,
    number: u64,
    branch: &str,
) {
    if let Err(e) = crate::mcp_server::merge_item_metadata(conn, &item.id, |merged| {
        // `pr.ready` (see `draft::persist_pr_ready`) survives a re-run of
        // `done` that finds the same PR again; a different PR number is a
        // different PR, whose readiness is unknown.
        let ready = merged
            .get("pr")
            .filter(|pr| pr["number"].as_u64() == Some(number))
            .is_some_and(|pr| pr["ready"] == true);
        let mut pr = serde_json::json!({ "number": number, "branch": branch });
        if ready {
            pr["ready"] = serde_json::Value::Bool(true);
        }
        merged.insert("pr".into(), pr);
    }) {
        eprintln!(
            "worktree: could not persist PR identity for item {}: {e}",
            item.id
        );
    }
}

/// Best-effort wrapper around `merge_and_persist_pr_identity` for
/// production callers, which have no database connection of their own to
/// hand in: opens the shared backend db directly, same as `cli::review`'s
/// performance-review path. A failure to even open the db must not stop
/// `push_and_open_pr` from returning the PR it already found/created.
fn persist_pr_identity(item: &agentflare_backend::item::Item, number: u64, branch: &str) {
    match agentflare_backend::db::open_db(&crate::vent::paths::backend_db_path()) {
        Ok(conn) => merge_and_persist_pr_identity(&conn, item, number, branch),
        Err(e) => eprintln!(
            "worktree: could not open backend db to persist PR identity for item {}: {e}",
            item.id
        ),
    }
}

/// True if `existing` should be trusted as *this item's own* PR on its
/// branch rather than an unrelated closed/merged PR that happens to reuse
/// the same branch name -- shared by `push_and_open_pr`'s pre-create lookup
/// and its post-create-failure recheck. Branch names get reused across items
/// over time, and `find_existing` matches on branch name alone, so an
/// unrelated, already-merged PR from a past item can share this branch's
/// name (item #63). An open match gets no exemption: an open PR tagged for
/// another item must not be recorded as this item's authoritative PR identity
/// (item #595).
fn is_own_pr(
    existing: &crate::github::models::PullRequest,
    item: &agentflare_backend::item::Item,
) -> bool {
    crate::github::pulls::marks_this_item(existing.body.as_deref(), item.sequence_id, &item.id)
}

/// Rechecks `find_existing` once after `pulls::create` fails on `branch` --
/// see `push_and_open_pr`'s `create` `Err` arm for why this matters (item
/// #261). Split out purely so this specific retry-and-trust behavior is
/// exercisable against a mock GitHub server: `push_and_open_pr` itself isn't
/// unit-testable end to end since it also needs a real git remote and
/// `Client::new()`'s live credentials.
fn recover_pr_after_failed_create(
    client: &crate::github::Client,
    repo: &RepoId,
    branch: &str,
    item: &agentflare_backend::item::Item,
) -> Option<crate::github::models::PullRequest> {
    match crate::github::pulls::find_existing(client, repo, branch) {
        Ok(Some(existing)) if reusable_own_pr(&existing, item) => Some(existing),
        _ => None,
    }
}

/// What `push_and_open_pr` achieved -- `item_done` has to tell "a PR can
/// never result here, by configuration" apart from "this attempt failed and
/// a retry may succeed", which the old bare `Option<String>` couldn't: a
/// repo with no GitHub remote or no credentials errored `done` forever.
/// The PR `push_and_open_pr` created or found, as much of it as `item_done`
/// needs afterwards: the URL for its response, and what flipping a draft to
/// ready takes (`draft::mark_pr_ready`) without a second lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenedPr {
    pub url: String,
    pub number: u64,
    pub node_id: Option<String>,
    pub draft: bool,
}

impl From<crate::github::models::PullRequest> for OpenedPr {
    fn from(pr: crate::github::models::PullRequest) -> Self {
        OpenedPr {
            url: pr.html_url,
            number: pr.number,
            node_id: pr.node_id,
            draft: pr.draft,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PrOutcome {
    /// A PR exists for this item's branch (freshly created, or already
    /// open/merged from an earlier `done`).
    Opened(OpenedPr),
    /// Nothing to publish: no worktree was ever created, or the branch never
    /// diverged from its target.
    NothingToPush,
    /// A PR is impossible for configuration reasons -- no `origin` remote at
    /// all, a non-GitHub remote, or no GitHub credentials. `pushed` says
    /// whether the branch did reach `origin`. Retrying can't change this.
    NoPrPossible { pushed: bool, reason: String },
    /// A push or GitHub call failed in a way a retry may fix (network, rate
    /// limit, rejected push).
    Failed(String),
}

/// Pushes `item`'s isolated worktree branch and opens a PR against
/// `target_branch` — the `done`-side counterpart to `create_worktree`.
/// Deliberately never merges: unreviewed code should never land on the
/// target branch automatically, so the worktree/branch are left in place
/// for the PR to actually get reviewed and merged. Never errors itself;
/// every failure is classified into a `PrOutcome` for `item_done` to act on.
pub fn push_and_open_pr(
    item: &agentflare_backend::item::Item,
    agent: &str,
    repo_root: &Path,
    target_branch: &str,
    progress: Option<&ProgressSender>,
    summary: Option<&str>,
) -> PrOutcome {
    let worktree_path = repo_root
        .join(".worktrees")
        .join("task")
        .join(item.sequence_id.to_string());
    if !worktree_path.exists() {
        return PrOutcome::NothingToPush;
    }
    // No `origin` at all: there is nowhere to push, and never will be until
    // someone configures one -- not a failure worth retrying.
    if flare_git_core::shell::run_in_opt(repo_root, &["remote", "get-url", "origin"]).is_none() {
        eprintln!(
            "worktree: no origin remote configured, skipping push/PR for item {}",
            item.id
        );
        return PrOutcome::NoPrPossible {
            pushed: false,
            reason: "no origin remote is configured".into(),
        };
    }
    let Some(branch) = flare_git_core::worktree::push_branch(
        item,
        repo_root,
        target_branch,
        as_progress(progress),
    ) else {
        // `push_branch` returns `None` both for "nothing to push" and for a
        // push that failed; divergence tells the two apart.
        return if branch_diverged(item, repo_root, target_branch) {
            PrOutcome::Failed("git push failed (see server logs)".into())
        } else {
            PrOutcome::NothingToPush
        };
    };
    if let Some(p) = progress {
        p.send(0.5, Some(1.0), Some("Creating PR...".into()));
    }
    let machine = crate::github::bridge::config::machine_label();
    let repo = match RepoId::resolve_from_remote(repo_root) {
        Some(r) => r,
        None => {
            eprintln!(
                "worktree: origin is not a GitHub remote, skipping PR for item {}",
                item.id
            );
            return PrOutcome::NoPrPossible {
                pushed: true,
                reason: "origin is not a GitHub remote".into(),
            };
        }
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "worktree: no GitHub credentials, skipping PR for item {}: {e}",
                item.id
            );
            return PrOutcome::NoPrPossible {
                pushed: true,
                reason: format!("no GitHub credentials: {e}"),
            };
        }
    };
    // The issue the GitHub bridge adopted this item from lives in the
    // bridge's repo, which is normally -- but not necessarily -- this one.
    let issue_repo = crate::github::bridge::config::resolve_project_repo(repo_root)
        .ok()
        .flatten();
    let mut body = pr_body(&item.id, summary);
    if let Some(closes) = closes_issue_line(item, issue_repo.as_ref(), &repo) {
        body.push_str("\n\n");
        body.push_str(&closes);
    }
    let body = format!(
        "{body}\n\n{}",
        pr_footer(agent, &machine, item.sequence_id, &item.id)
    );
    open_pr_for_pushed_branch(
        &client,
        &repo,
        item,
        &branch,
        target_branch,
        &body,
        &machine,
        progress,
    )
}

/// Whether an existing PR on this item's branch should be returned instead
/// of opening a new one: it must be this item's own PR, and not one that was
/// closed without merging -- a closed attempt is dead (the review sweep
/// sends its item back for a fresh try), so reusing it would bounce the item
/// straight back into "in_review" on a PR that can never merge.
fn reusable_own_pr(
    existing: &crate::github::models::PullRequest,
    item: &agentflare_backend::item::Item,
) -> bool {
    is_own_pr(existing, item) && (existing.state != "closed" || existing.merged_at.is_some())
}

/// The GitHub half of `push_and_open_pr`, once the branch is pushed and a
/// repo/client are in hand -- split out so it can be driven against a mock
/// server.
///
/// Prefers the item's recorded `metadata.pr.number` (a direct `pulls::get`,
/// no branch search at all), then a head-filtered `find_existing`. A lookup
/// *error* is reported as `Failed` without creating anything: under a rate
/// limit, "couldn't check" used to fall through to `create` and open a
/// duplicate PR every time `done` was retried.
#[allow(clippy::too_many_arguments)]
fn open_pr_for_pushed_branch(
    client: &crate::github::Client,
    repo: &RepoId,
    item: &agentflare_backend::item::Item,
    branch: &str,
    target_branch: &str,
    body: &str,
    machine: &str,
    progress: Option<&ProgressSender>,
) -> PrOutcome {
    let found_existing = |existing: crate::github::models::PullRequest| {
        persist_pr_identity(item, existing.number, branch);
        if let Some(p) = progress {
            p.send(1.0, Some(1.0), Some("PR already exists".into()));
        }
        PrOutcome::Opened(existing.into())
    };
    if let Some(number) = pr_number_from_metadata(item) {
        match crate::github::pulls::get(client, repo, number) {
            Ok(existing)
                if reusable_own_pr(&existing, item)
                    && existing.head.as_ref().is_none_or(|h| h.git_ref == branch) =>
            {
                return found_existing(existing);
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!(
                    "worktree: could not look up recorded PR #{number} for item {}: {e}",
                    item.id
                );
                return PrOutcome::Failed(format!("could not look up PR #{number}: {e}"));
            }
        }
    }
    // Check for an existing PR on this branch before opening a new one.
    // GitHub's own API only rejects a duplicate while the existing PR is
    // still open -- once it's merged, a second PR against the same branch is
    // perfectly legal to create, which is exactly how a `done` re-run on an
    // already-merged item ended up opening a redundant PR (2026-07-25).
    //
    // A match is only trusted as *this item's own* prior PR when its body
    // carries this item's marker -- branch names get reused across items
    // over time, so an unrelated, already-merged PR from a past item can
    // share this branch's name (item #63).
    match crate::github::pulls::find_existing(client, repo, branch) {
        Ok(Some(existing)) if reusable_own_pr(&existing, item) => {
            return found_existing(existing);
        }
        Ok(Some(existing)) => {
            eprintln!(
                "worktree: found a {} PR #{} on branch {branch} but it isn't a reusable PR of item {} -- opening a new one",
                existing.state, existing.number, item.id
            );
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!(
                "worktree: could not check for an existing PR for item {}: {e} -- not creating one blind",
                item.id
            );
            return PrOutcome::Failed(format!("could not check for an existing PR: {e}"));
        }
    }
    // Opened as a draft: CI starts, but nobody is asked to review and
    // nothing can merge until `item_done` has recorded the item as
    // in_review and flips it ready (`draft::mark_pr_ready`) -- so a `done`
    // that fails between here and there leaves a PR nobody is chasing yet.
    let title = conventional_pr_title(&item.name);
    let create = |draft: bool| {
        crate::github::pulls::create(
            client,
            repo,
            &title,
            branch,
            target_branch,
            Some(body),
            draft,
        )
    };
    let created = match create(true) {
        Err(e) if crate::github::pulls::drafts_unsupported(&e) => {
            eprintln!(
                "worktree: {repo} does not support draft PRs ({e}); opening item {}'s PR as \
                 ready for review",
                item.id
            );
            create(false)
        }
        other => other,
    };
    match created {
        Ok(pr) => {
            persist_pr_identity(item, pr.number, branch);
            if let Err(e) = crate::github::issues::add_labels(
                client,
                repo,
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
            PrOutcome::Opened(pr.into())
        }
        Err(e) => {
            // `create` fails this way when two workstations independently
            // dispatched the same item raced: both lookups above ran before
            // either had created a PR yet, both called `create`, and GitHub
            // accepted only one (rejecting the loser with a duplicate-branch
            // error). Recheck so the loser links up with the winner's PR
            // instead of leaving the item without one (item #261).
            eprintln!(
                "worktree: PR creation failed for item {}: {e} -- rechecking for a racing PR",
                item.id
            );
            match recover_pr_after_failed_create(client, repo, branch, item) {
                Some(existing) => found_existing(existing),
                None => PrOutcome::Failed(format!("PR creation failed: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    include!("mod_tests.rs");
}
