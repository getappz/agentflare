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

/// The set of PR numbers already tracked by *any* item in a project (not
/// just `in_review` ones) via `metadata.pr.number` -- `run_review_sweep`
/// diffs this against a repo's open PRs to find ones `discover_untracked_prs`
/// still needs to create an item for.
pub(crate) fn tracked_pr_numbers(
    items: &[agentflare_backend::item::Item],
) -> std::collections::HashSet<u64> {
    items.iter().filter_map(pr_number_from_metadata).collect()
}

/// Creates an `in_review` item for every open, non-draft PR in `repo` not
/// already in `known_pr_numbers` -- PRs opened outside the `item done` flow
/// (by hand, or by an agent working ad hoc) would otherwise sit invisible to
/// `run_review_sweep` forever, even once a human adds the approval label,
/// since the sweep only ever iterates items it already knows about. Gated on
/// `github::is_trusted_author_association` the same way issue intake is
/// (`github::bridge::tick::is_trusted_author`): only the repo's own
/// owner/member/collaborator PRs get auto-tracked -- an external
/// contributor's PR must go through a human before it enters this pipeline
/// at all. The synthesized item's `metadata.pr` shape matches
/// `merge_and_persist_pr_identity` exactly, so every downstream sweep step
/// (CI check, self-repair, branch update, merge) treats it identically to a
/// normal item. Returns the number of items created; soft-fails to 0 on any
/// GitHub error, same as this file's other PR-lookup functions.
pub(crate) fn discover_untracked_prs(
    conn: &rusqlite::Connection,
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    project_id: &str,
    in_review_state_id: &str,
    known_pr_numbers: &std::collections::HashSet<u64>,
) -> usize {
    let prs = match crate::github::pulls::list(client, repo, "open") {
        Ok(prs) => prs,
        Err(e) => {
            eprintln!("worktree: could not list open PRs for {repo}: {e}");
            return 0;
        }
    };
    let mut created = 0;
    for pr in prs {
        if pr.draft
            || known_pr_numbers.contains(&pr.number)
            || !crate::github::is_trusted_author_association(&pr.author_association)
        {
            continue;
        }
        let Some(branch) = pr.head.as_ref().map(|h| h.git_ref.clone()) else {
            continue;
        };
        let description = pr
            .body
            .clone()
            .unwrap_or_else(|| format!("Auto-tracked PR: {}", pr.html_url));
        let metadata = serde_json::json!({"pr": {"number": pr.number, "branch": branch}});
        let input = agentflare_backend::item::CreateItem {
            project_id: project_id.to_string(),
            state_id: in_review_state_id.to_string(),
            name: pr.title,
            description: Some(description),
            priority: None,
            parent_id: None,
            assignee_agent: None,
            sort_order: None,
            external_source: None,
            external_id: None,
            metadata: Some(metadata.to_string()),
            label_ids: vec![],
            assignee_ids: vec![],
            dependency_ids: vec![],
        };
        match agentflare_backend::item::create(conn, input) {
            Ok(_) => created += 1,
            Err(e) => eprintln!("worktree: could not create item for PR #{}: {e}", pr.number),
        }
    }
    created
}

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
                && crate::github::pulls::marks_item(pr.body.as_deref(), item.sequence_id)
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
        Ok(Some(pr)) if crate::github::pulls::marks_item(pr.body.as_deref(), item.sequence_id) => {
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
pub enum PrCiStatus {
    Merged,
    Failing(Vec<String>),
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
                    if crate::github::pulls::marks_item(pr.body.as_deref(), item.sequence_id) =>
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
        PrCiStatus::Passing {
            number: pr.number,
            labels: pr.labels.into_iter().map(|l| l.name).collect(),
        }
    } else {
        PrCiStatus::Failing(failed)
    }
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
    match crate::github::pulls::update_branch(&client, &repo, number) {
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
fn pr_footer(agent: &str, machine: &str, sequence_id: i64) -> String {
    format!("---\n_Opened by `{agent}` on **{machine}** for item #{sequence_id} via agentflare._")
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
        Ok(Some(existing))
            if existing.state == "open"
                || crate::github::pulls::marks_item(existing.body.as_deref(), item.sequence_id) =>
        {
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

    /// Mirrors what `amannn/action-semantic-pull-request` (configured in
    /// `.github/workflows/pr-title.yml`) checks: title starts with one of
    /// `CONVENTIONAL_TYPES`, optionally scoped, followed by `: `.
    fn satisfies_pr_title_check(title: &str) -> bool {
        let Some((head, rest)) = title.split_once(':') else {
            return false;
        };
        if !rest.starts_with(' ') || rest.trim().is_empty() {
            return false;
        }
        let type_token = head.split('(').next().unwrap_or(head);
        CONVENTIONAL_TYPES.contains(&type_token)
    }

    #[test]
    fn conventional_pr_title_passes_through_an_already_valid_prefix() {
        assert_eq!(
            conventional_pr_title("fix: correct off-by-one in pagination"),
            "fix: correct off-by-one in pagination"
        );
    }

    #[test]
    fn conventional_pr_title_lowercases_an_existing_valid_prefix() {
        assert_eq!(
            conventional_pr_title("Feat: support nested worktrees"),
            "feat: support nested worktrees"
        );
    }

    #[test]
    fn conventional_pr_title_preserves_an_existing_scope() {
        assert_eq!(
            conventional_pr_title("Fix(worktree): don't leak file handles"),
            "fix(worktree): don't leak file handles"
        );
    }

    #[test]
    fn conventional_pr_title_maps_bugfix_prefix_to_fix() {
        assert_eq!(
            conventional_pr_title(
                "Bugfix: detect_review_only doesn't classify design-spec tasks as no-code"
            ),
            "fix: detect_review_only doesn't classify design-spec tasks as no-code"
        );
    }

    #[test]
    fn conventional_pr_title_maps_feature_prefix_to_feat() {
        assert_eq!(
            conventional_pr_title(
                "Feature: add WorkflowStatus::Waiting for Sleep/SleepUntil/WaitEvent suspension"
            ),
            "feat: add WorkflowStatus::Waiting for Sleep/SleepUntil/WaitEvent suspension"
        );
    }

    #[test]
    fn conventional_pr_title_falls_back_to_a_keyword_scan_for_plain_english() {
        assert_eq!(
            conventional_pr_title("improve documentation for the review command"),
            "docs: improve documentation for the review command"
        );
    }

    #[test]
    fn conventional_pr_title_defaults_to_chore_when_nothing_matches() {
        assert_eq!(
            conventional_pr_title("bump vendored dependency versions"),
            "chore: bump vendored dependency versions"
        );
    }

    #[test]
    fn conventional_pr_title_always_satisfies_the_ci_check() {
        for name in [
            "fix: correct off-by-one in pagination",
            "Feat: support nested worktrees",
            "Fix(worktree): don't leak file handles",
            "Bugfix: detect_review_only doesn't classify design-spec tasks as no-code",
            "Feature: add WorkflowStatus::Waiting for Sleep/SleepUntil/WaitEvent suspension",
            "stop the daemon from double-dispatching the same item",
            "bump vendored dependency versions",
            "Add support for Cline CLI",
            "Refactor the worktree module",
            "Improve docs for the review command",
        ] {
            let title = conventional_pr_title(name);
            assert!(
                satisfies_pr_title_check(&title),
                "title {title:?} (from {name:?}) does not satisfy the PR title check"
            );
        }
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

    fn item_with_metadata(sequence_id: i64, metadata: &str) -> agentflare_backend::item::Item {
        agentflare_backend::item::Item {
            id: format!("item-{sequence_id}"),
            project_id: "p".into(),
            state_id: "s".into(),
            name: "n".into(),
            description: String::new(),
            priority: "none".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id,
            sort_order: 0.0,
            started_at: None,
            completed_at: None,
            archived_at: None,
            external_source: None,
            external_id: None,
            metadata: metadata.into(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
        }
    }

    #[test]
    fn pr_number_from_metadata_reads_the_stored_pr_number() {
        let item = item_with_metadata(191, r#"{"pr":{"number":619,"branch":"b"}}"#);
        assert_eq!(pr_number_from_metadata(&item), Some(619));
    }

    #[test]
    fn pr_number_from_metadata_is_none_when_absent() {
        let item = item_with_metadata(191, r#"{"size":"S"}"#);
        assert_eq!(pr_number_from_metadata(&item), None);
    }

    #[test]
    fn pr_number_from_metadata_is_none_for_non_object_metadata() {
        let item = item_with_metadata(191, "not json");
        assert_eq!(pr_number_from_metadata(&item), None);
    }

    // Item #191: `check_merge` reported "PR not merged yet" for a PR that
    // was demonstrably merged, because `resolve_item_task_branch`
    // reconstructed a branch name that no longer matched what the PR was
    // actually opened against. With `metadata.pr.number` set, `is_pr_merged`
    // must go straight to `pulls::get` by number and never touch branch
    // reconstruction at all.
    #[test]
    fn is_pr_merged_uses_metadata_pr_number_even_when_branch_name_would_not_resolve() {
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"{"number":619,"html_url":"u","state":"closed","title":"t","merged_at":"2026-08-20T00:00:00Z"}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let item = item_with_metadata(
            191,
            r#"{"pr":{"number":619,"branch":"task/191-opencode-agentflare-work-dispatch-doesn"}}"#,
        );

        assert!(is_pr_merged_impl(
            &item,
            Path::new("/does/not/exist"),
            &client,
            &repo
        ));

        let reqs = server.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, "/repos/o/r/pulls/619");
    }

    #[test]
    fn is_pr_merged_falls_back_to_branch_heuristic_when_metadata_pr_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let item = item_with_metadata(63, "{}");
        let branch = flare_git_core::worktree::resolve_item_task_branch(&item, dir.path());
        let body = format!(
            "---\\n_Opened by `claude-code` on **box** for item #{} via agentflare._",
            item.sequence_id
        );
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                &format!(
                    r#"[{{"number":5,"html_url":"u","state":"closed","title":"t","merged_at":"2026-08-20T00:00:00Z","head":{{"ref":"{branch}","sha":"abc"}},"body":"{body}"}}]"#
                ),
            ),
        ]);
        let client = server.client(None);
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };

        assert!(is_pr_merged_impl(&item, dir.path(), &client, &repo));

        let reqs = server.requests();
        assert_eq!(
            reqs[0].path,
            "/repos/o/r/pulls?state=all&per_page=100&page=1"
        );
    }

    #[test]
    fn pr_ci_status_uses_metadata_pr_number_to_report_merged() {
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"{"number":619,"html_url":"u","state":"closed","title":"t","merged_at":"2026-08-20T00:00:00Z"}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let item = item_with_metadata(191, r#"{"pr":{"number":619,"branch":"whatever"}}"#);

        let status = pr_ci_status_impl(&item, Path::new("/does/not/exist"), &client, &repo);

        assert!(matches!(status, PrCiStatus::Merged));
        assert_eq!(server.requests()[0].path, "/repos/o/r/pulls/619");
    }

    #[test]
    fn pr_ci_status_reports_behind_before_ever_fetching_check_runs() {
        // GitHub's own mergeable_state == "behind": mergeable, no conflict,
        // just missing commits the base branch gained since. Only one
        // request should fire -- the PR fetch itself -- confirming this is
        // checked before `list_check_runs` would otherwise be called (item
        // #197's follow-up: no point fetching CI status for checks the
        // branch update is about to invalidate anyway).
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"{"number":621,"html_url":"u","state":"open","title":"t","mergeable":true,"mergeable_state":"behind"}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let item = item_with_metadata(195, r#"{"pr":{"number":621,"branch":"whatever"}}"#);

        let status = pr_ci_status_impl(&item, Path::new("/does/not/exist"), &client, &repo);

        assert!(matches!(status, PrCiStatus::Behind { number: 621 }));
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn merge_and_persist_pr_identity_merges_without_clobbering_existing_metadata_keys() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let ws = agentflare_backend::workspace::create(
            &conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Test".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = agentflare_backend::project::create(
            &conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id.clone(),
                name: "Test".into(),
                identifier: "T".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let state = agentflare_backend::state::list_by_project(&conn, &proj.id)
            .unwrap()
            .into_iter()
            .find(|s| s.is_default)
            .unwrap();
        let item = agentflare_backend::item::create(
            &conn,
            agentflare_backend::item::CreateItem {
                project_id: proj.id,
                state_id: state.id,
                name: "Test Item".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: Some(r#"{"size":"S"}"#.into()),
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap();

        merge_and_persist_pr_identity(&conn, &item, 619, "task/191-slug");

        let updated = agentflare_backend::item::get(&conn, &item.id).unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
        assert_eq!(metadata["size"], "S");
        assert_eq!(metadata["pr"]["number"], 619);
        assert_eq!(metadata["pr"]["branch"], "task/191-slug");
    }

    fn test_project_with_in_review_state(conn: &rusqlite::Connection) -> (String, String) {
        let ws = agentflare_backend::workspace::create(
            conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Test".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = agentflare_backend::project::create(
            conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id.clone(),
                name: "Test".into(),
                identifier: "T".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let in_review = agentflare_backend::state::list_by_project(conn, &proj.id)
            .unwrap()
            .into_iter()
            .find(|s| s.group_name == "in_review")
            .unwrap();
        (proj.id, in_review.id)
    }

    #[test]
    fn tracked_pr_numbers_collects_from_items_with_pr_metadata() {
        let items = vec![
            item_with_metadata(1, r#"{"pr":{"number":10,"branch":"a"}}"#),
            item_with_metadata(2, r#"{"pr":{"number":20,"branch":"b"}}"#),
            item_with_metadata(3, "{}"),
        ];

        let tracked = tracked_pr_numbers(&items);

        assert_eq!(tracked, [10, 20].into_iter().collect());
    }

    #[test]
    fn discover_untracked_prs_creates_an_item_for_an_untracked_open_pr() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":42,"html_url":"u","state":"open","title":"Fix thing","body":"does the fix","head":{"ref":"fix/thing","sha":"abc"},"author_association":"OWNER"}]"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let known = std::collections::HashSet::new();

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &known,
        );

        assert_eq!(created, 1);
        let items = agentflare_backend::item::list_by_project(&conn, &project_id).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Fix thing");
        assert_eq!(items[0].state_id, in_review_state_id);
        assert_eq!(items[0].description, "does the fix");
        let metadata: serde_json::Value = serde_json::from_str(&items[0].metadata).unwrap();
        assert_eq!(metadata["pr"]["number"], 42);
        assert_eq!(metadata["pr"]["branch"], "fix/thing");
    }

    #[test]
    fn discover_untracked_prs_skips_a_pr_already_tracked_by_an_item() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":42,"html_url":"u","state":"open","title":"Fix thing","head":{"ref":"fix/thing","sha":"abc"},"author_association":"OWNER"}]"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let known: std::collections::HashSet<u64> = [42].into_iter().collect();

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &known,
        );

        assert_eq!(created, 0);
        assert!(
            agentflare_backend::item::list_by_project(&conn, &project_id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn discover_untracked_prs_skips_a_pr_from_an_untrusted_author() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":42,"html_url":"u","state":"open","title":"Fix thing","head":{"ref":"fix/thing","sha":"abc"},"author_association":"CONTRIBUTOR"}]"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let known = std::collections::HashSet::new();

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &known,
        );

        assert_eq!(created, 0);
        assert!(
            agentflare_backend::item::list_by_project(&conn, &project_id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn discover_untracked_prs_skips_draft_prs() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":42,"html_url":"u","state":"open","title":"WIP","draft":true,"head":{"ref":"wip","sha":"abc"},"author_association":"OWNER"}]"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let known = std::collections::HashSet::new();

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &known,
        );

        assert_eq!(created, 0);
    }
}
