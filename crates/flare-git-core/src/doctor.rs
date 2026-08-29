use std::path::Path;

use crate::audit;
use crate::classify::{Disposition, Event};
use crate::shell::{run_in as run_git_in, run_in_ok};

/// How many of the most recent commit/push scope-checks to look at when
/// deciding whether scope enforcement is sustained-broken vs. one-off.
pub const SCOPE_CHECK_HEALTH_WINDOW: usize = 10;
/// Crashes within that window at or above this count count as "sustained"
/// (an `agentflare` binary crashing on nearly every scope-check has, from
/// the caller's perspective, disabled scope enforcement) rather than the
/// occasional isolated crash (e.g. a mid-upgrade binary swap) that's
/// expected and not actionable on its own.
pub const SCOPE_CHECK_HEALTH_THRESHOLD: usize = 3;

/// Looks for a sustained scope-check breakage in the git-shim's audit
/// events (item #483 bug #2 followup): the shim fails OPEN when
/// `agentflare git scope-check` crashes mid-run (a single crash must not
/// brick a legitimate push), logging `Disposition::ScopeCheckError` each
/// time it does. A one-off crash is routine noise, but if most of the
/// *recent* commit/push attempts crashed, scope enforcement is effectively
/// off even though every one of those git operations appeared to succeed.
///
/// Only looks at `commit`/`push` events (the only subcommands that ever
/// invoke scope-check) and only at the most recent `window` of them,
/// oldest-first `events` slice (as returned by `audit::read_events`)
/// scanned from the tail. Returns `None` when fewer than `threshold` of
/// them crashed.
#[must_use]
pub fn sustained_scope_check_failure(
    events: &[Event],
    window: usize,
    threshold: usize,
) -> Option<String> {
    let recent: Vec<&Event> = events
        .iter()
        .rev()
        .filter(|e| matches!(e.subcommand.as_str(), "commit" | "push"))
        .take(window)
        .collect();
    let crashes = recent
        .iter()
        .filter(|e| matches!(e.disposition, Disposition::ScopeCheckError { .. }))
        .count();
    if crashes < threshold {
        return None;
    }
    Some(format!(
        "scope-check crashed on {crashes} of the last {} commit/push invocations -- \
         scope enforcement is effectively OFF, not just occasionally flaky. Check the \
         'agentflare' binary on PATH (mid-upgrade? corrupted install?) and see \
         ~/.agentflare/audit/git.jsonl for crash details.",
        recent.len()
    ))
}

/// Reads `~/.agentflare/audit/git.jsonl` and pushes a
/// [`sustained_scope_check_failure`] violation onto `report`, if any. A
/// missing/unreadable log is silently skipped (best-effort, matching this
/// module's other doctor-report enrichment).
pub fn append_scope_check_violation(report: &mut DoctorReport) {
    let Some(audit_path) = audit::default_path("git.jsonl") else {
        return;
    };
    let Ok(events) = audit::read_events::<Event>(&audit_path) else {
        return;
    };
    if let Some(violation) = sustained_scope_check_failure(
        &events,
        SCOPE_CHECK_HEALTH_WINDOW,
        SCOPE_CHECK_HEALTH_THRESHOLD,
    ) {
        report.violations.push(violation);
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct LaneHealth {
    pub name: String,
    pub path: String,
    pub sequence_id: Option<i64>,
    pub flags: Vec<HealthFlag>,
    /// True for the main/canonical worktree (`git worktree list`'s first
    /// entry) as opposed to a linked worktree. `reclaim` must never delete
    /// this lane, regardless of flags or `--force` — see the 2026-07-25
    /// incident where treating it like any other lane wiped the entire
    /// checkout, including every linked worktree nested under it.
    pub is_main_worktree: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthFlag {
    Dirty,
    Stale {
        days: u64,
    },
    MissingWorktree,
    DuplicateBranch {
        other_paths: Vec<String>,
    },
    MissingUpstream,
    Orphaned {
        item_state: String,
    },
    /// Claiming session's process is no longer alive. Not implemented yet —
    /// needs a cross-platform pid-liveness check with no existing precedent
    /// in this codebase (ponytail: add when a real zombie-worktree incident
    /// occurs; until then `scan` never pushes this variant).
    Zombie,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DoctorReport {
    pub lanes: Vec<LaneHealth>,
    pub violations: Vec<String>,
    pub summary: Summary,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Summary {
    pub total: usize,
    pub flagged: usize,
    pub dirty: usize,
    pub stale: usize,
    pub missing_worktree: usize,
    pub duplicate_branch: usize,
    pub missing_upstream: usize,
    pub orphaned: usize,
    pub zombie: usize,
    pub clean: usize,
}

pub fn scan(
    repo_root: &Path,
    staleness_days: u64,
    item_states: &std::collections::HashMap<String, String>,
) -> DoctorReport {
    let mut lanes = Vec::new();
    let mut violations = Vec::new();
    let mut branch_lanes: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();

    let worktrees = list_worktrees(repo_root);
    let git_worktree_entries = parse_worktree_list(&worktrees);

    for (entry_idx, entry) in git_worktree_entries.iter().enumerate() {
        // `git worktree list` always lists the main/canonical worktree
        // first, followed by linked worktrees — this is documented,
        // stable git behavior, not an assumption specific to this repo.
        let is_main_worktree = entry_idx == 0;
        let path = Path::new(&entry.path);
        let name = entry
            .branch
            .as_deref()
            .or(entry.detached_at.as_deref())
            .or_else(|| path.file_name().and_then(|n| n.to_str()))
            .unwrap_or("unknown")
            .to_string();

        let mut flags: Vec<HealthFlag> = Vec::new();
        let sequence_id = name
            .strip_prefix("task/")
            .and_then(|s| s.parse::<i64>().ok());

        if let Some(ref branch) = entry.branch {
            branch_lanes
                .entry(branch.clone())
                .or_default()
                .push((name.clone(), entry.path.clone()));

            if !has_upstream(repo_root, branch) {
                flags.push(HealthFlag::MissingUpstream);
            }
        }

        if !path.exists() {
            flags.push(HealthFlag::MissingWorktree);
            lanes.push(LaneHealth {
                name: name.clone(),
                path: entry.path.clone(),
                sequence_id,
                flags,
                is_main_worktree,
            });
            continue;
        }

        if is_dirty(path) {
            flags.push(HealthFlag::Dirty);
        }

        if let Some(days) = days_since_last_commit(path)
            && days >= staleness_days
        {
            flags.push(HealthFlag::Stale { days });
        }

        if let Some(group) = sequence_id.and_then(|seq| item_states.get(&seq.to_string()))
            && matches!(group.as_str(), "completed" | "cancelled")
        {
            flags.push(HealthFlag::Orphaned {
                item_state: group.clone(),
            });
        }

        lanes.push(LaneHealth {
            name,
            path: entry.path.clone(),
            sequence_id,
            flags,
            is_main_worktree,
        });
    }

    for (branch, instances) in &branch_lanes {
        if instances.len() > 1 {
            let other_paths: Vec<String> = instances.iter().map(|(_, p)| p.clone()).collect();
            for (inst_name, _inst_path) in instances {
                if let Some(lane) = lanes.iter_mut().find(|l| l.name == *inst_name) {
                    lane.flags.push(HealthFlag::DuplicateBranch {
                        other_paths: other_paths.clone(),
                    });
                }
            }
            violations.push(format!(
                "duplicate-branch: '{}' in {} worktrees",
                branch,
                instances.len()
            ));
        }
    }

    let total = lanes.len();
    let flagged = lanes.iter().filter(|l| !l.flags.is_empty()).count();
    let clean = lanes.iter().filter(|l| l.flags.is_empty()).count();
    let dirty_count = lanes
        .iter()
        .filter(|l| l.flags.iter().any(|f| matches!(f, HealthFlag::Dirty)))
        .count();
    let stale_count = lanes
        .iter()
        .filter(|l| {
            l.flags
                .iter()
                .any(|f| matches!(f, HealthFlag::Stale { .. }))
        })
        .count();
    let missing_wt_count = lanes
        .iter()
        .filter(|l| {
            l.flags
                .iter()
                .any(|f| matches!(f, HealthFlag::MissingWorktree))
        })
        .count();
    let dup_count = lanes
        .iter()
        .filter(|l| {
            l.flags
                .iter()
                .any(|f| matches!(f, HealthFlag::DuplicateBranch { .. }))
        })
        .count();
    let missing_up_count = lanes
        .iter()
        .filter(|l| {
            l.flags
                .iter()
                .any(|f| matches!(f, HealthFlag::MissingUpstream))
        })
        .count();
    let orphaned_count = lanes
        .iter()
        .filter(|l| {
            l.flags
                .iter()
                .any(|f| matches!(f, HealthFlag::Orphaned { .. }))
        })
        .count();
    let zombie_count = lanes
        .iter()
        .filter(|l| l.flags.iter().any(|f| matches!(f, HealthFlag::Zombie)))
        .count();

    if dirty_count > 5 {
        violations.push(format!("{} dirty worktrees (threshold: >5)", dirty_count));
    }
    if stale_count > 3 {
        violations.push(format!("{} stale worktrees (threshold: >3)", stale_count));
    }

    DoctorReport {
        lanes,
        violations,
        summary: Summary {
            total,
            flagged,
            dirty: dirty_count,
            stale: stale_count,
            missing_worktree: missing_wt_count,
            duplicate_branch: dup_count,
            missing_upstream: missing_up_count,
            orphaned: orphaned_count,
            zombie: zombie_count,
            clean,
        },
    }
}

pub fn reclaim(repo_root: &Path, report: &DoctorReport, force: bool) -> Vec<String> {
    reclaim_scoped(repo_root, report, force, None)
}

/// Same as `reclaim`, but when `target` is `Some`, only the lane whose name
/// or path matches it is even considered -- every other lane is skipped
/// outright, regardless of its flags or `force`.
///
/// **Why this exists (2026-08-16 incident):** an agent ran
/// `reclaim(repo_root, &report, true)` (unscoped, `force=true`) to fix a
/// single broken worktree (`task/479`) and it deleted two OTHER, unrelated
/// dirty worktrees (`task/110`, `task/473`) along with it -- their
/// uncommitted work was lost. `force=true` bypasses the one thing (the
/// `Dirty` flag) that would normally protect a worktree from deletion, so an
/// unscoped call to it is repo-wide by construction: it does not just "also
/// allow" the target lane to be force-deleted, it force-deletes *every*
/// dirty lane in the report. Callers that mean "fix this one worktree" must
/// pass `target`; leaving it `None` is only correct for a genuine repo-wide
/// sweep (e.g. a scheduled hygiene pass), never for repairing one broken lane.
pub fn reclaim_scoped(
    repo_root: &Path,
    report: &DoctorReport,
    force: bool,
    target: Option<&str>,
) -> Vec<String> {
    let mut reclaimed = Vec::new();
    // Whether any lane was reclaimed at all -- a single `git worktree prune`
    // at the end clears every dangling `.git/worktrees/<name>` admin entry
    // this pass created or inherited. It used to run only after a successful
    // `remove_dir_all`, so a `MissingWorktree` lane (directory already gone
    // some other way) was reported reclaimed while its admin entry survived:
    // `git worktree list` kept calling it prunable and `git branch -d` kept
    // failing with "used by worktree", with no way out because plain
    // `git worktree prune` is denied by the shim.
    let mut needs_prune = false;
    for lane in &report.lanes {
        // Never reclaim the main/canonical worktree, full stop -- not even
        // under `--force`. `git worktree remove` itself refuses to ever
        // remove the main worktree; treating this lane like any other
        // linked worktree previously let `remove_dir_all` wipe the entire
        // checkout (and, since linked worktrees live nested under it,
        // every other lane along with it) whenever the main worktree
        // happened to pick up a flag like `MissingUpstream` or `Stale`.
        if lane.is_main_worktree {
            continue;
        }
        if let Some(t) = target
            && lane.name != t
            && lane.path != t
        {
            continue;
        }
        // A lane with no flags at all is healthy -- nothing to reclaim.
        // Without this, `lane.flags.iter().all(..)` below is vacuously true
        // on an empty iterator, so a perfectly clean lane would pass the
        // "is_safe" check and get deleted just for being named as `target`,
        // even with `force=false`.
        if lane.flags.is_empty() {
            continue;
        }
        let has_dirty = lane.flags.iter().any(|f| matches!(f, HealthFlag::Dirty));
        if has_dirty && !force {
            continue;
        }
        let is_safe = lane.flags.iter().all(|f| match f {
            HealthFlag::Dirty => force,
            HealthFlag::Stale { .. } => true,
            HealthFlag::MissingWorktree => true,
            HealthFlag::DuplicateBranch { .. } => true,
            HealthFlag::MissingUpstream => true,
            HealthFlag::Orphaned { .. } => true,
            HealthFlag::Zombie => true,
        });
        if !is_safe {
            continue;
        }
        let path = Path::new(&lane.path);
        if path.exists() {
            // Snapshot the lane's OWN contents: `snapshot_before(repo_root)`
            // stages from the main checkout, where `.worktrees/` is excluded,
            // so it captured nothing of the directory about to be deleted.
            if let Err(e) = crate::snapshot::snapshot_worktree_before(
                repo_root,
                path,
                &format!("doctor reclaim {}", lane.name),
            ) {
                eprintln!(
                    "doctor: snapshot failed before reclaiming {}: {}",
                    lane.name, e
                );
                continue;
            }
            match std::fs::remove_dir_all(path) {
                Ok(()) => {
                    needs_prune = true;
                    reclaimed.push(lane.name.clone());
                }
                // A lane whose directory could not be deleted is NOT
                // reclaimed -- deliberately no push here, and no prune
                // either, since its admin entry still points at a live dir.
                Err(e) => {
                    eprintln!("doctor: failed to reclaim '{}': {}", lane.name, e);
                }
            }
        } else {
            needs_prune = true;
            reclaimed.push(lane.name.clone());
        }
    }
    crate::shell::prune_worktree_metadata_if(repo_root, needs_prune);
    reclaimed
}

pub fn format_text(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str("=== flare doctor ===\n\n");
    for lane in &report.lanes {
        out.push_str(&format!("  {}  ({})\n", lane.name, lane.path));
        for flag in &lane.flags {
            let line = match flag {
                HealthFlag::Dirty => "    ⚠ dirty: uncommitted changes".into(),
                HealthFlag::Stale { days } => format!("    ⏳ stale: untouched >{}d", days),
                HealthFlag::MissingWorktree => "    ✗ missing-worktree: path gone".into(),
                HealthFlag::DuplicateBranch { other_paths } => {
                    format!("    ⚠ duplicate-branch: also at {}", other_paths.join(", "))
                }
                HealthFlag::MissingUpstream => "    ⚠ missing-upstream: no remote".into(),
                HealthFlag::Orphaned { item_state } => {
                    format!("    🗑 orphaned: item {item_state}")
                }
                HealthFlag::Zombie => "    💀 zombie: session pid dead".into(),
            };
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push('\n');
    out.push_str(&format!(
        "Summary: {} total, {} flagged, {} clean\n",
        report.summary.total, report.summary.flagged, report.summary.clean
    ));
    if !report.violations.is_empty() {
        out.push_str("Violations:\n");
        for v in &report.violations {
            out.push_str(&format!("  - {v}\n"));
        }
    }
    out
}

pub fn format_json(report: &DoctorReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".into())
}

pub fn format_markdown(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str("# flare doctor\n\n");
    out.push_str("| Lane | Path | Flags |\n|------|------|-------|\n");
    for lane in &report.lanes {
        let flag_str = lane
            .flags
            .iter()
            .map(|f| match f {
                HealthFlag::Dirty => "dirty",
                HealthFlag::Stale { .. } => "stale",
                HealthFlag::MissingWorktree => "missing-worktree",
                HealthFlag::DuplicateBranch { .. } => "duplicate-branch",
                HealthFlag::MissingUpstream => "missing-upstream",
                HealthFlag::Orphaned { .. } => "orphaned",
                HealthFlag::Zombie => "zombie",
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            lane.name, lane.path, flag_str
        ));
    }
    out.push('\n');
    out.push_str(&format!(
        "**Summary:** {} total, {} flagged, {} clean\n\n",
        report.summary.total, report.summary.flagged, report.summary.clean
    ));
    if !report.violations.is_empty() {
        out.push_str("**Violations:**\n");
        for v in &report.violations {
            out.push_str(&format!("- {v}\n"));
        }
    }
    out
}

struct WorktreeEntry {
    path: String,
    branch: Option<String>,
    /// Set instead of `branch` for a detached-HEAD worktree — kept separate
    /// so detached lanes don't get run through branch-only checks
    /// (`has_upstream`, duplicate-branch grouping) with a fake "branch name".
    detached_at: Option<String>,
}

fn list_worktrees(repo_root: &Path) -> String {
    run_git_in(repo_root, &["worktree", "list", "--porcelain"]).unwrap_or_default()
}

fn parse_worktree_list(output: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_branch: Option<String> = None;
    let mut current_detached: Option<String> = None;
    for line in output.lines() {
        if line.trim().is_empty() {
            if let Some(path) = current_path.take() {
                entries.push(WorktreeEntry {
                    path,
                    branch: current_branch.take(),
                    detached_at: current_detached.take(),
                });
            }
        } else if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(prev_path) = current_path.take() {
                entries.push(WorktreeEntry {
                    path: prev_path,
                    branch: current_branch.take(),
                    detached_at: current_detached.take(),
                });
            }
            current_path = Some(path.to_string());
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            current_branch = Some(branch.to_string());
        } else if let Some(rev) = line.strip_prefix("HEAD ")
            && current_branch.is_none()
        {
            current_detached = Some(format!("detached at {rev}"));
        }
    }
    if let Some(path) = current_path.take() {
        entries.push(WorktreeEntry {
            path,
            branch: current_branch.take(),
            detached_at: current_detached.take(),
        });
    }
    entries
}

/// `run_in_ok` only reflects exit status, and `git status --porcelain`
/// exits 0 whether the tree is dirty or clean — so dirtiness has to come
/// from the (trimmed) stdout being non-empty instead.
///
/// Reports `false` (not dirty) when `git status` itself fails, so this is a
/// fail-open diagnostic helper — fine for `doctor`'s reporting use, but
/// wrong for anything that gates a policy decision on cleanliness. Callers
/// making an allow/deny call (e.g. the protected-branch checkout guard in
/// `classify.rs`) must use [`is_dirty_checked`] instead, which fails closed.
pub(crate) fn is_dirty(path: &Path) -> bool {
    is_dirty_checked(path).unwrap_or(false)
}

/// Same check as [`is_dirty`], but `None` when `git status` itself fails —
/// callers that gate a policy decision on tree cleanliness should treat
/// `None` as dirty (fail closed) rather than defaulting to "clean".
pub(crate) fn is_dirty_checked(path: &Path) -> Option<bool> {
    run_git_in(path, &["status", "--porcelain"])
        .ok()
        .map(|out| !out.is_empty())
}

/// Days since the worktree's HEAD commit, or `None` if it can't be read
/// (e.g. no commits yet, or `git log` fails).
fn days_since_last_commit(path: &Path) -> Option<u64> {
    let out = run_git_in(path, &["log", "-1", "--format=%ct"]).ok()?;
    let commit_epoch: i64 = out.trim().parse().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let secs = (now - commit_epoch).max(0);
    Some((secs / 86_400) as u64)
}

fn has_upstream(repo_root: &Path, branch: &str) -> bool {
    run_in_ok(
        repo_root,
        &[
            "rev-parse",
            "--abbrev-ref",
            &format!("{branch}@{{upstream}}"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope_event(subcommand: &str, disposition: Disposition) -> Event {
        Event {
            subcommand: subcommand.to_string(),
            args: vec![],
            disposition,
        }
    }

    fn crash_event(subcommand: &str) -> Event {
        scope_event(
            subcommand,
            Disposition::ScopeCheckError {
                message: "crashed".to_string(),
            },
        )
    }

    #[test]
    fn no_events_is_healthy() {
        assert!(sustained_scope_check_failure(&[], 10, 3).is_none());
    }

    #[test]
    fn a_single_isolated_crash_is_not_sustained() {
        let events = vec![
            crash_event("push"),
            scope_event("push", Disposition::Passthrough),
            scope_event("commit", Disposition::Passthrough),
            scope_event("push", Disposition::Passthrough),
        ];
        assert!(
            sustained_scope_check_failure(&events, 10, 3).is_none(),
            "one crash among several healthy checks must not alert"
        );
    }

    #[test]
    fn crashes_at_or_above_threshold_within_the_window_are_sustained() {
        let events = vec![
            scope_event("push", Disposition::Passthrough),
            crash_event("commit"),
            crash_event("push"),
            crash_event("commit"),
        ];
        let msg = sustained_scope_check_failure(&events, 10, 3)
            .expect("3 crashes at threshold 3 must alert");
        assert!(msg.contains("3 of the last 4"), "{msg}");
        assert!(
            msg.contains("scope enforcement is effectively OFF"),
            "{msg}"
        );
    }

    #[test]
    fn crashes_outside_the_window_are_ignored() {
        // 3 crashes total, but only the most recent `window` (2) are
        // considered, and neither of those crashed.
        let events = vec![
            crash_event("push"),
            crash_event("commit"),
            crash_event("push"),
            scope_event("commit", Disposition::Passthrough),
            scope_event("push", Disposition::Passthrough),
        ];
        assert!(
            sustained_scope_check_failure(&events, 2, 1).is_none(),
            "crashes outside the recent window must not count"
        );
    }

    #[test]
    fn non_scope_checked_subcommands_do_not_dilute_the_window() {
        // `fetch`/`status`/etc. never invoke scope-check, so they must not
        // count toward the window and quietly push real crashes out of it.
        let mut events = vec![crash_event("push"), crash_event("commit")];
        for _ in 0..20 {
            events.push(scope_event("status", Disposition::Passthrough));
        }
        events.push(crash_event("push"));
        let msg = sustained_scope_check_failure(&events, 10, 3)
            .expect("3 commit/push crashes must still alert despite 20 unrelated events");
        assert!(msg.contains("3 of the last 3"), "{msg}");
    }

    #[test]
    fn parse_worktree_list_two_entries_with_branches() {
        let output = "worktree /repo1\nHEAD a\nbranch refs/heads/main\n\nworktree /repo2\nHEAD b\nbranch refs/heads/feature\n";
        let entries = parse_worktree_list(output);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[1].branch.as_deref(), Some("feature"));
    }

    #[test]
    fn parse_worktree_list_detached() {
        let output = "worktree /repo\nHEAD abc\n";
        let entries = parse_worktree_list(output);
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].branch.is_none(),
            "detached HEAD must not be reported as a branch"
        );
        assert!(
            entries[0]
                .detached_at
                .as_deref()
                .unwrap_or("")
                .contains("detached")
        );
    }

    #[test]
    fn parse_worktree_list_detached_is_excluded_from_branch_checks() {
        // A detached lane must not populate `branch`, since scan() gates
        // has_upstream/branch_lanes checks on `entry.branch.is_some()`.
        let output = "worktree /repo\nHEAD abc123\n";
        let entries = parse_worktree_list(output);
        assert_eq!(entries[0].branch, None);
    }

    #[test]
    fn parse_worktree_list_empty() {
        assert!(parse_worktree_list("").is_empty());
    }

    #[test]
    fn scan_returns_summary() {
        let report = scan(
            Path::new("/nonexistent"),
            14,
            &std::collections::HashMap::new(),
        );
        assert_eq!(report.summary.total, 0);
    }

    #[test]
    fn days_since_last_commit_none_outside_a_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(days_since_last_commit(tmp.path()), None);
    }

    #[test]
    fn days_since_last_commit_zero_right_after_committing() {
        let repo = crate::shell::test_support::init_repo_with_branch("main");
        assert_eq!(days_since_last_commit(&repo.path), Some(0));
    }

    #[test]
    fn is_dirty_false_outside_a_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(!is_dirty(tmp.path()));
    }

    #[test]
    fn is_dirty_false_on_a_clean_repo() {
        let repo = crate::shell::test_support::init_repo_with_branch("main");
        assert!(!is_dirty(&repo.path));
    }

    #[test]
    fn is_dirty_true_with_an_untracked_file() {
        let repo = crate::shell::test_support::init_repo_with_branch("main");
        std::fs::write(repo.path.join("untracked.txt"), "x").unwrap();
        assert!(is_dirty(&repo.path));
    }

    #[test]
    fn scan_does_not_flag_a_fresh_clean_worktree_as_stale() {
        let repo = crate::shell::test_support::init_repo_with_branch("main");
        let report = scan(&repo.path, 14, &std::collections::HashMap::new());
        assert_eq!(
            report.lanes.len(),
            1,
            "expected exactly the repo's own worktree, got: {:?}",
            report.lanes
        );
        let lane = &report.lanes[0];
        assert!(
            !lane
                .flags
                .iter()
                .any(|f| matches!(f, HealthFlag::Stale { .. })),
            "a worktree committed seconds ago must not be flagged stale, got: {:?}",
            lane.flags
        );
        assert!(!lane.flags.iter().any(|f| matches!(f, HealthFlag::Dirty)));
    }

    #[test]
    fn reclaim_never_deletes_the_main_worktree_even_when_flagged() {
        // Regression for the 2026-07-25 incident: `reclaim` treated the
        // main worktree like any other lane, and since linked worktrees
        // live nested under it (mirroring this repo's own
        // `.worktrees/<name>` layout), deleting it wiped everything.
        let repo = crate::shell::test_support::init_repo_with_branch("main");
        // Mirrors this repo's own `.gitignore` (`/.worktrees/`) -- without
        // it, the main lane's `git status` would see the linked worktree's
        // directory itself as an untracked path and flag Dirty, which
        // isn't what production looks like and isn't what this test means
        // to exercise.
        std::fs::write(repo.path.join(".gitignore"), ".worktrees/\n").unwrap();
        crate::shell::run_in(&repo.path, &["add", ".gitignore"]).unwrap();
        crate::shell::run_in(&repo.path, &["commit", "-m", "gitignore worktrees"]).unwrap();
        let linked_path = repo.path.join(".worktrees").join("linked");
        std::fs::create_dir_all(repo.path.join(".worktrees")).unwrap();
        crate::shell::run_in(
            &repo.path,
            &[
                "worktree",
                "add",
                "-b",
                "linked-branch",
                linked_path.to_str().unwrap(),
            ],
        )
        .unwrap();
        // Make the linked worktree dirty so it is NOT individually
        // reclaim-eligible under force=false -- if it disappears anyway,
        // that can only be because deleting the main worktree cascaded
        // into it (linked worktrees are nested underneath), not because it
        // was legitimately reclaimed on its own.
        std::fs::write(linked_path.join("scratch.txt"), "uncommitted\n").unwrap();

        let report = scan(&repo.path, 14, &std::collections::HashMap::new());
        let main_lane = report
            .lanes
            .iter()
            .find(|l| l.is_main_worktree)
            .expect("scan must report the main worktree as a lane");
        // Neither branch has a configured upstream in this local-only test
        // repo -- the main lane picks up MissingUpstream naturally, exactly
        // the kind of ordinary flag that used to make it reclaim-eligible.
        assert!(
            main_lane
                .flags
                .iter()
                .any(|f| matches!(f, HealthFlag::MissingUpstream)),
            "test setup assumption: main lane should be flagged, got {:?}",
            main_lane.flags
        );
        assert!(
            !main_lane
                .flags
                .iter()
                .any(|f| matches!(f, HealthFlag::Dirty)),
            "main lane must be clean here to exercise the pre-fix vulnerable path, got {:?}",
            main_lane.flags
        );
        let main_name = main_lane.name.clone();
        let linked_lane = report
            .lanes
            .iter()
            .find(|l| !l.is_main_worktree)
            .expect("scan must report the linked worktree as a lane");
        assert!(
            linked_lane
                .flags
                .iter()
                .any(|f| matches!(f, HealthFlag::Dirty)),
            "test setup assumption: linked lane should be dirty, got {:?}",
            linked_lane.flags
        );

        let reclaimed = reclaim(&repo.path, &report, false);

        assert!(
            repo.path.exists(),
            "main worktree must survive reclaim even when flagged"
        );
        assert!(
            repo.path.join(".git").exists(),
            "main worktree's .git must survive reclaim"
        );
        assert!(
            linked_path.exists(),
            "linked worktree nested under the main one must survive -- it's \
             dirty so wouldn't be reclaimed on its own, and must not be \
             cascaded away by main-worktree deletion"
        );
        assert!(
            !reclaimed.contains(&main_name),
            "main worktree must never be reported as reclaimed"
        );
    }

    #[test]
    fn reclaim_prunes_the_admin_entry_of_an_already_deleted_worktree() {
        // Vent #469: `git worktree prune` used to run only after a
        // successful `remove_dir_all`, so a lane whose directory was
        // already gone -- exactly what `MissingWorktree` describes -- got
        // reported as reclaimed with its `.git/worktrees/<name>` admin
        // entry left dangling. `git branch -d` then kept failing with
        // "used by worktree", and the shim denies plain `git worktree
        // prune`, so there was no way out.
        let repo = crate::shell::test_support::init_repo_with_branch("main");
        std::fs::write(repo.path.join(".gitignore"), ".worktrees/\n").unwrap();
        crate::shell::run_in(&repo.path, &["add", ".gitignore"]).unwrap();
        crate::shell::run_in(&repo.path, &["commit", "-m", "gitignore worktrees"]).unwrap();
        let linked_path = repo.path.join(".worktrees").join("linked");
        crate::shell::run_in(
            &repo.path,
            &[
                "worktree",
                "add",
                "-b",
                "linked-branch",
                linked_path.to_str().unwrap(),
            ],
        )
        .unwrap();
        // Delete the directory out from under git, the way an agent (or an
        // `rm -rf`) would -- leaving the admin entry behind.
        std::fs::remove_dir_all(&linked_path).unwrap();

        let report = scan(&repo.path, 14, &std::collections::HashMap::new());
        let lane = report
            .lanes
            .iter()
            .find(|l| !l.is_main_worktree)
            .expect("scan must still report the lane whose directory is gone");
        assert!(
            lane.flags
                .iter()
                .any(|f| matches!(f, HealthFlag::MissingWorktree)),
            "test setup assumption: lane should be flagged missing-worktree, got {:?}",
            lane.flags
        );

        let reclaimed = reclaim(&repo.path, &report, false);
        assert!(
            reclaimed.iter().any(|n| n == "linked-branch"),
            "the missing lane should be reported reclaimed, got {reclaimed:?}"
        );

        let listed =
            crate::shell::run_in(&repo.path, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(
            !listed.contains("prunable"),
            "reclaim must clear the dangling admin entry, still got: {listed}"
        );
        // The payoff: the branch is deletable again.
        crate::shell::run_in(&repo.path, &["branch", "-d", "linked-branch"])
            .expect("branch must be deletable once its worktree entry is pruned");
    }

    #[test]
    fn scoped_force_reclaim_touches_only_the_named_target() {
        // Regression for the 2026-08-16 incident: `reclaim(.., force=true)`
        // with no target deleted every dirty lane, not just the one the
        // caller meant to fix -- wiping two unrelated worktrees' uncommitted
        // work. `reclaim_scoped` with `target` set must leave every other
        // dirty lane alone even under `force=true`.
        let repo = crate::shell::test_support::init_repo_with_branch("main");
        std::fs::write(repo.path.join(".gitignore"), ".worktrees/\n").unwrap();
        crate::shell::run_in(&repo.path, &["add", ".gitignore"]).unwrap();
        crate::shell::run_in(&repo.path, &["commit", "-m", "gitignore worktrees"]).unwrap();

        let mut dirty_paths = Vec::new();
        for name in ["target", "bystander-a", "bystander-b"] {
            let path = repo.path.join(".worktrees").join(name);
            std::fs::create_dir_all(repo.path.join(".worktrees")).unwrap();
            crate::shell::run_in(
                &repo.path,
                &["worktree", "add", "-b", name, path.to_str().unwrap()],
            )
            .unwrap();
            std::fs::write(path.join("scratch.txt"), "uncommitted\n").unwrap();
            dirty_paths.push((name.to_string(), path));
        }

        let report = scan(&repo.path, 14, &std::collections::HashMap::new());
        assert_eq!(
            report
                .lanes
                .iter()
                .filter(|l| l.flags.iter().any(|f| matches!(f, HealthFlag::Dirty)))
                .count(),
            3,
            "test setup assumption: all three linked lanes should be dirty, got {:?}",
            report.lanes
        );

        let reclaimed = reclaim_scoped(&repo.path, &report, true, Some("target"));

        assert_eq!(
            reclaimed,
            vec!["target".to_string()],
            "only the targeted lane should be reported reclaimed"
        );
        for (name, path) in &dirty_paths {
            if name == "target" {
                assert!(!path.exists(), "targeted dirty lane must be deleted");
            } else {
                assert!(
                    path.exists(),
                    "bystander lane '{name}' must survive a scoped force reclaim, \
                     even though it is also flagged dirty"
                );
            }
        }
    }

    #[test]
    fn targeting_a_healthy_worktree_does_not_reclaim_it() {
        // CodeRabbit finding on PR #515: `lane.flags.iter().all(..)` is
        // vacuously true for a lane with no flags at all, so a perfectly
        // healthy worktree named as `target` was reclaim-eligible even with
        // `force=false` -- naming a lane was, by itself, enough to delete it.
        // Built by hand rather than via `scan()`: a real linked worktree in
        // a local-only test repo always picks up `MissingUpstream` (no
        // remote configured), so it can never actually reach zero flags
        // through `scan()` -- this constructs the exact zero-flag case
        // directly instead.
        let repo = crate::shell::test_support::init_repo_with_branch("main");
        let healthy_path = repo.path.join(".worktrees").join("healthy");
        std::fs::create_dir_all(&healthy_path).unwrap();
        std::fs::write(healthy_path.join("marker.txt"), "still here\n").unwrap();

        let report = DoctorReport {
            lanes: vec![
                LaneHealth {
                    name: "main".to_string(),
                    path: repo.path.to_string_lossy().to_string(),
                    sequence_id: None,
                    flags: vec![],
                    is_main_worktree: true,
                },
                LaneHealth {
                    name: "healthy".to_string(),
                    path: healthy_path.to_string_lossy().to_string(),
                    sequence_id: None,
                    flags: vec![],
                    is_main_worktree: false,
                },
            ],
            violations: vec![],
            summary: Summary {
                total: 2,
                flagged: 0,
                dirty: 0,
                stale: 0,
                missing_worktree: 0,
                duplicate_branch: 0,
                missing_upstream: 0,
                orphaned: 0,
                zombie: 0,
                clean: 2,
            },
        };

        let reclaimed = reclaim_scoped(&repo.path, &report, true, Some("healthy"));

        assert!(
            reclaimed.is_empty(),
            "a healthy lane must never be reclaimed just for being targeted"
        );
        assert!(healthy_path.exists(), "healthy worktree must survive");
    }
}
