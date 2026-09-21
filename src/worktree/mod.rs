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
pub fn relabel_pr_completed(item: &agentflare_backend::item::Item, repo_root: &Path) {
    let branch = flare_git_core::worktree::resolve_item_task_branch(item, repo_root);
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return;
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(_) => return,
    };
    let pr = match crate::github::pulls::find_existing(&client, &repo, &branch) {
        Ok(Some(pr))
            if crate::github::pulls::marks_this_item(
                pr.body.as_deref(),
                item.sequence_id,
                &item.id,
            ) =>
        {
            pr
        }
        Ok(Some(_)) | Ok(None) => return,
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
/// polls per item: merged (promote), failing (self-repair), CI-green with a
/// human approval label attached (auto-merge, item #194), cleanly behind the
/// base branch with no conflict (update-branch, item #197's follow-up), or
/// nothing actionable yet. `Unknown` covers every soft-fail case
/// `is_pr_merged` above also treats as "not merged yet" -- no credentials,
/// no resolvable remote, no PR found, or a lookup error -- since the
/// caller's fallback is simply to poll again next tick.
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
    /// API round-trip just to re-fetch labels.
    Passing {
        number: u64,
        labels: Vec<String>,
    },
    /// GitHub's own `mergeable_state == "behind"` -- mergeable, no conflict,
    /// just missing commits the base branch has gained since this PR was
    /// opened/last updated. Checked before CI status is even fetched: a
    /// stale-but-behind PR's existing check runs are stale too, and re-fetching
    /// them here would be wasted work the branch update is about to
    /// invalidate anyway.
    Behind {
        number: u64,
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
    if pr.mergeable == Some(true) && pr.mergeable_state.as_deref() == Some("behind") {
        return PrCiStatus::Behind { number: pr.number };
    }
    if pr.mergeable == Some(false) && pr.mergeable_state.as_deref() == Some("dirty") {
        return PrCiStatus::Conflicting { number: pr.number };
    }
    let Some(sha) = pr.head.as_ref().map(|h| h.sha.clone()) else {
        return PrCiStatus::Unknown;
    };
    let checks = match crate::github::actions::list_check_runs(client, repo, &sha) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "worktree: could not fetch check status for item {}: {e}",
                item.id
            );
            return PrCiStatus::Unknown;
        }
    };
    let mergeable_state = pr.mergeable_state.clone();
    decide_from_checks(
        pr.number,
        &checks,
        pr.labels.into_iter().map(|l| l.name).collect(),
        mergeable_state.as_deref(),
    )
}

/// The part of the CI-status decision tree that only needs check-run data
/// (merged/behind are decided from the PR itself before this is reached) --
/// shared verbatim by `pr_ci_status_impl`'s per-PR REST fetch and
/// `pr_ci_status_from_batch`'s GraphQL-batch fetch, so the two fetch paths
/// can never quietly disagree on what a given set of check runs means.
fn decide_from_checks(
    number: u64,
    checks: &[crate::github::models::CheckRun],
    labels: Vec<String>,
    mergeable_state: Option<&str>,
) -> PrCiStatus {
    let summary = crate::github::mcp::checks_wait_summary(checks, 0);
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
    if !failed.is_empty() {
        return PrCiStatus::Failing {
            number,
            checks: failed,
            labels,
        };
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
    if matches!(mergeable_state, Some("blocked") | Some("unknown")) {
        return PrCiStatus::Pending;
    }
    PrCiStatus::Passing { number, labels }
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
    if data.mergeable == Some(true) && data.mergeable_state.as_deref() == Some("behind") {
        return PrCiStatus::Behind { number };
    }
    if data.mergeable == Some(false) && data.mergeable_state.as_deref() == Some("dirty") {
        return PrCiStatus::Conflicting { number };
    }
    decide_from_checks(
        number,
        &data.checks,
        data.labels.clone(),
        data.mergeable_state.as_deref(),
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
pub fn update_stale_branch(repo_root: &Path, number: u64) -> bool {
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return false;
    };
    let Ok(client) = crate::github::Client::new() else {
        return false;
    };
    update_branch_pr(&client, &repo, number)
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
/// `merge` call already does.
fn update_branch_pr(client: &crate::github::Client, repo: &RepoId, number: u64) -> bool {
    match crate::github::pulls::update_branch(client, repo, number) {
        Ok(()) => true,
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

/// Merges `{"pr": {"number": N, "branch": "..."}}` into `item`'s existing
/// metadata (without clobbering unrelated keys like `size`/`workflow_run_id`)
/// and persists it -- the identity `is_pr_merged`/`pr_ci_status` read back
/// directly instead of reconstructing the branch name to rediscover the
/// same PR (item #191: that reconstruction drifted from the PR's real
/// branch, and `check_merge` reported "not merged yet" for a PR that had
/// actually merged). Coerces non-object metadata to an empty object first,
/// same defensive stance as `work_item_pipeline::persist_run_id` -- `Value`
/// indexing panics assigning into anything that isn't already `Object`.
fn merge_and_persist_pr_identity(
    conn: &rusqlite::Connection,
    item: &agentflare_backend::item::Item,
    number: u64,
    branch: &str,
) {
    let mut merged = serde_json::from_str::<serde_json::Value>(&item.metadata)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .map(serde_json::Value::Object)
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    merged["pr"] = serde_json::json!({ "number": number, "branch": branch });
    if let Err(e) = agentflare_backend::item::update(
        conn,
        &item.id,
        agentflare_backend::item::UpdateItem {
            metadata: Some(merged.to_string()),
            ..Default::default()
        },
    ) {
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
/// name (item #63). An open match is always trusted regardless of body,
/// since GitHub itself would reject creating a genuine duplicate against it
/// anyway.
fn is_own_pr(
    existing: &crate::github::models::PullRequest,
    item: &agentflare_backend::item::Item,
) -> bool {
    existing.state == "open"
        || crate::github::pulls::marks_this_item(
            existing.body.as_deref(),
            item.sequence_id,
            &item.id,
        )
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
        Ok(Some(existing)) if is_own_pr(&existing, item) => Some(existing),
        _ => None,
    }
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
        pr_footer(agent, &machine, item.sequence_id, &item.id)
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
    //
    // A closed/merged match is only trusted as *this item's own* prior PR
    // when its body carries this item's marker -- branch names get reused
    // across items over time, and `find_existing` matches on branch name
    // alone, so an unrelated, already-merged PR from a past item can share
    // this branch's name (item #63: that stale match got returned as
    // `pr_url`, which made `in_review` true and skipped the
    // `nothing_was_ever_committed` safety net for real, uncommitted work).
    // An open match is always trusted regardless of its body, since GitHub
    // itself would reject creating a genuine duplicate against it anyway.
    match crate::github::pulls::find_existing(&client, &repo, &branch) {
        Ok(Some(existing)) if is_own_pr(&existing, item) => {
            persist_pr_identity(item, existing.number, &branch);
            if let Some(p) = progress {
                p.send(1.0, Some(1.0), Some("PR already exists".into()));
            }
            return Some(existing.html_url);
        }
        Ok(Some(existing)) => {
            eprintln!(
                "worktree: found a {} PR #{} on branch {branch} but it isn't item {}'s own PR -- opening a new one",
                existing.state, existing.number, item.id
            );
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
        &conventional_pr_title(&item.name),
        &branch,
        target_branch,
        Some(&body),
    ) {
        Ok(pr) => {
            persist_pr_identity(item, pr.number, &branch);
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
            // `create` fails this way when two workstations independently
            // dispatched the same item raced: both `find_existing` checks
            // above ran before either had created a PR yet, both saw
            // nothing, both called `create`, and GitHub accepted only one
            // (rejecting the loser with a duplicate-branch error). Without
            // this recheck the loser used to just log and give up here,
            // leaving the item without a linked PR even though the winner's
            // PR -- the one the item actually needs to track -- already
            // exists. This is the failure mode `push_and_open_pr`'s own
            // success branch left unguarded (item #261: PR #688 ended up
            // carrying two different workstations' `beacon:` labels because
            // both reached the `Ok(pr)` branch above instead of one of them
            // landing here and linking up with the other's PR instead).
            eprintln!(
                "worktree: PR creation failed for item {}: {e} -- rechecking for a racing PR",
                item.id
            );
            match recover_pr_after_failed_create(&client, &repo, &branch, item) {
                Some(existing) => {
                    persist_pr_identity(item, existing.number, &branch);
                    if let Some(p) = progress {
                        p.send(1.0, Some(1.0), Some("PR already exists".into()));
                    }
                    Some(existing.html_url)
                }
                None => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    include!("mod_tests.rs");
}
