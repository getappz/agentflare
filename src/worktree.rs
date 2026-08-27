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

/// Checks whether `item`'s branch already has a merged PR — the promotion
/// signal `check_merge` uses to move an item out of "in_review" (item
/// #420). Soft-fails like `push_and_open_pr`: no GitHub credentials, no
/// resolvable remote, or a lookup failure all just report "not merged yet"
/// rather than erroring, since the caller's fallback is simply to check
/// again later.
///
/// `find_existing` matches on branch name alone, and branch names get
/// reused across items over time, so a match is only trusted as this
/// item's own PR when `marks_item` confirms it -- otherwise an unrelated,
/// already-merged PR from a past item would fool `check_merge` into
/// promoting this item off someone else's merge (item #63).
pub fn is_pr_merged(item: &agentflare_backend::item::Item, repo_root: &Path) -> bool {
    let branch = flare_git_core::worktree::resolve_item_task_branch(item, repo_root);
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return false;
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(_) => return false,
    };
    match crate::github::pulls::find_existing(&client, &repo, &branch) {
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
/// human approval label attached (auto-merge, item #194), or nothing
/// actionable yet. `Unknown` covers every soft-fail case `is_pr_merged`
/// above also treats as "not merged yet" -- no credentials, no resolvable
/// remote, no PR found, or a lookup error -- since the caller's fallback is
/// simply to poll again next tick.
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
    Unknown,
}

/// Same "total>0 && not pending" gate `cli::git::wait_for_checks` polls on,
/// applied once instead of in a loop -- the sweep itself provides the retry
/// cadence across ticks.
///
/// Same branch-reuse hazard as `is_pr_merged`: a `find_existing` match is
/// only trusted once `marks_item` confirms it's this item's own PR, so an
/// unrelated PR sharing the branch name can't report its CI status as this
/// item's (item #63).
pub fn pr_ci_status(item: &agentflare_backend::item::Item, repo_root: &Path) -> PrCiStatus {
    let branch = flare_git_core::worktree::resolve_item_task_branch(item, repo_root);
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return PrCiStatus::Unknown;
    };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(_) => return PrCiStatus::Unknown,
    };
    let pr = match crate::github::pulls::find_existing(&client, &repo, &branch) {
        Ok(Some(pr)) if crate::github::pulls::marks_item(pr.body.as_deref(), item.sequence_id) => {
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
    };
    if pr.merged_at.is_some() {
        return PrCiStatus::Merged;
    }
    let Some(sha) = pr.head.as_ref().map(|h| h.sha.clone()) else {
        return PrCiStatus::Unknown;
    };
    let checks = match crate::github::actions::list_check_runs(&client, &repo, &sha) {
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
}
