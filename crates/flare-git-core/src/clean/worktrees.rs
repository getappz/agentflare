//! Which worktrees (and loose merged branches) can go.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::merged::{BranchInfo, Verdict};
use super::{Item, Kind, ScanInput, Skipped};
use crate::shell::run_in;
use crate::worktree::{AGENTFLARE_LOCK_REASON, audit_orphans, dir_size};
use agentflare_config::paths;
use flare_process::cwd::LiveProc;

/// The first live process whose cwd is at or under `dir`.
pub(super) fn occupant<'a>(dir: &Path, live: &'a [LiveProc]) -> Option<&'a LiveProc> {
    live.iter().find(|p| p.cwd.starts_with(dir))
}

fn rel_label(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Why a merged worktree still cannot be removed, if anything. Used at scan
/// and again at apply, so a worktree that was dirtied, entered, locked or
/// claimed after the plan is refused the same way.
pub(super) fn blocker(
    live: &[LiveProc],
    claimed_items: &HashSet<String>,
    path: &Path,
    canon: &Path,
    locked: Option<&str>,
) -> Option<String> {
    if !path.is_dir() {
        return Some("worktree directory is missing (run `git worktree prune`)".into());
    }
    // `--untracked-files` is explicit: `status.showUntrackedFiles=no` would
    // otherwise hide untracked work and let it be deleted.
    match run_in(path, &["status", "--porcelain", "--untracked-files=normal"]) {
        Ok(out) if out.is_empty() => {}
        Ok(_) => return Some("uncommitted changes".into()),
        Err(e) => return Some(format!("cannot read status: {e}")),
    }
    if let Some(p) = occupant(canon, live) {
        return Some(format!("live process: {} (pid {})", p.name, p.pid));
    }
    // The invoking shell is excluded from `live`, so check it separately.
    if std::env::current_dir().is_ok_and(|cwd| paths::is_within(canon, &cwd)) {
        return Some("the current directory is inside it".into());
    }
    match locked {
        None => None,
        // Any lock we did not place is someone's deliberate pin.
        Some(reason) if reason != AGENTFLARE_LOCK_REASON => Some(format!(
            "locked: {}",
            if reason.is_empty() {
                "no reason given"
            } else {
                reason
            }
        )),
        Some(_) => {
            let dir_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            claimed_items
                .contains(dir_name)
                .then(|| "claimed by a live session".into())
        }
    }
}

/// Items for removable worktrees (each carries its branch), for orphaned
/// worktree directories, and for merged branches checked out nowhere; plus
/// what was left alone and why.
pub(super) fn candidates(
    input: &ScanInput,
    repo_root: &Path,
    branches: Vec<BranchInfo>,
) -> (Vec<Item>, Vec<Skipped>) {
    let (mut items, mut skipped) = (Vec::new(), Vec::new());
    let canon_root = paths::canonical(repo_root);
    let mut by_name: HashMap<String, BranchInfo> =
        branches.into_iter().map(|b| (b.name.clone(), b)).collect();

    let listing = run_in(repo_root, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    for (index, entry) in crate::doctor::parse_worktree_list(&listing)
        .into_iter()
        .enumerate()
    {
        if index == 0 {
            // The main checkout is never a candidate, and its branch is not
            // loose either -- whichever worktree this runs from.
            if let Some(info) = entry.branch.and_then(|b| by_name.remove(&b))
                && input.opts.branches
            {
                skipped.push(Skipped {
                    label: info.name,
                    reason: match info.verdict {
                        Verdict::Skip(reason) => reason,
                        Verdict::Merged(_) => "checked out in the main worktree".into(),
                    },
                });
            }
            continue;
        }
        let path = Path::new(&entry.path);
        let label = rel_label(&canon_root, path);
        let Some(branch) = entry.branch else {
            skipped.push(Skipped {
                label,
                reason: "detached HEAD".into(),
            });
            continue;
        };
        // A checked-out branch is decided here, never as a loose branch:
        // git cannot delete it while its worktree exists.
        let Some(info) = by_name.remove(&branch) else {
            continue;
        };
        if !input.opts.worktrees {
            continue;
        }
        let canon = paths::canonical(path);
        let merged = match info.verdict {
            Verdict::Merged(reason) => reason,
            Verdict::Skip(reason) => {
                skipped.push(Skipped { label, reason });
                continue;
            }
        };
        let locked = entry.locked.as_deref();
        if let Some(reason) = blocker(input.live, input.claimed_items, path, &canon, locked) {
            skipped.push(Skipped { label, reason });
            continue;
        }
        items.push(Item {
            id: format!("worktree:{label}"),
            kind: Kind::Worktree,
            label,
            size_bytes: dir_size(&canon),
            path: Some(canon),
            branch: Some(branch),
            sha: Some(info.sha),
            reason: format!("clean · {merged}"),
        });
    }

    if input.opts.worktrees {
        for o in audit_orphans(repo_root, Some(input.claimed_items)) {
            items.push(Item {
                id: format!("orphan:{}", o.name),
                kind: Kind::Orphan,
                label: rel_label(&canon_root, &o.path),
                path: Some(o.path),
                branch: None,
                sha: None,
                size_bytes: o.size_bytes,
                reason: if o.has_broken_gitdir {
                    "orphaned · broken .git".into()
                } else {
                    "orphaned · on the default branch".into()
                },
            });
        }
    }

    if input.opts.branches {
        let mut loose: Vec<BranchInfo> = by_name.into_values().collect();
        loose.sort_by(|a, b| a.name.cmp(&b.name));
        for b in loose {
            match b.verdict {
                Verdict::Merged(reason) => items.push(Item {
                    id: format!("branch:{}", b.name),
                    kind: Kind::Branch,
                    label: b.name.clone(),
                    path: None,
                    branch: Some(b.name),
                    sha: Some(b.sha),
                    size_bytes: 0,
                    reason,
                }),
                Verdict::Skip(reason) => skipped.push(Skipped {
                    label: b.name,
                    reason,
                }),
            }
        }
    }
    (items, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::merged::classify_local;
    use crate::clean::{CleanOptions, Kind, NoPrLookup, ScanInput};
    use crate::shell::run_in;
    use crate::shell::test_support::{Repo, init_repo_with_branch};
    use agentflare_config::paths;
    use flare_process::cwd::LiveProc;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;

    const OUR_LOCK: &str = "agentflare: in use by work item";

    fn add_worktree(repo: &Repo, rel: &str, branch: &str) -> PathBuf {
        let path = repo.path.join(rel);
        let p = path.to_str().unwrap();
        run_in(&repo.path, &["worktree", "add", "-b", branch, p]).unwrap();
        paths::canonical(&path)
    }

    fn plan(
        repo: &Repo,
        claimed: &HashSet<String>,
        live: &[LiveProc],
    ) -> (Vec<Item>, Vec<Skipped>) {
        let opts = CleanOptions {
            branches: true,
            worktrees: true,
            ..Default::default()
        };
        let states = HashMap::new();
        let input = ScanInput {
            repo_root: Some(&repo.path),
            scan_root: &repo.path,
            opts: &opts,
            prs: &NoPrLookup,
            pr_lookup_available: true,
            claimed_items: claimed,
            item_states: &states,
            live,
        };
        let branches = classify_local(&input, &repo.path, "master");
        candidates(&input, &repo.path, branches)
    }

    fn reason_for<'a>(skipped: &'a [Skipped], needle: &str) -> &'a str {
        &skipped
            .iter()
            .find(|s| s.label.contains(needle))
            .unwrap_or_else(|| panic!("no skip for {needle}: {skipped:?}"))
            .reason
    }

    #[test]
    fn clean_merged_worktree_is_one_item_carrying_its_branch() {
        let repo = init_repo_with_branch("master");
        add_worktree(&repo, ".worktrees/task/7", "task/7-x");
        run_in(&repo.path, &["branch", "loose"]).unwrap();
        let (items, _) = plan(&repo, &HashSet::new(), &[]);
        let wt = items.iter().find(|i| i.kind == Kind::Worktree).unwrap();
        assert_eq!(wt.branch.as_deref(), Some("task/7-x"));
        assert!(wt.sha.is_some());
        assert!(items.iter().any(|i| i.id == "branch:loose"));
        assert!(
            !items.iter().any(|i| i.id == "branch:task/7-x"),
            "a checked-out branch is not a separate item"
        );
    }

    #[test]
    fn dirty_worktree_is_skipped() {
        let repo = init_repo_with_branch("master");
        let wt = add_worktree(&repo, ".worktrees/task/7", "task/7-x");
        std::fs::write(wt.join("scratch.txt"), "wip").unwrap();
        let (items, skipped) = plan(&repo, &HashSet::new(), &[]);
        assert!(items.iter().all(|i| i.kind != Kind::Worktree));
        assert_eq!(reason_for(&skipped, "task/7"), "uncommitted changes");
    }

    #[test]
    fn live_process_inside_blocks_removal() {
        let repo = init_repo_with_branch("master");
        let wt = add_worktree(&repo, ".worktrees/task/7", "task/7-x");
        let live = [LiveProc {
            pid: 42,
            name: "cargo".into(),
            cwd: wt.join("src"),
        }];
        let (items, skipped) = plan(&repo, &HashSet::new(), &live);
        assert!(items.iter().all(|i| i.kind != Kind::Worktree));
        assert_eq!(
            reason_for(&skipped, "task/7"),
            "live process: cargo (pid 42)"
        );
    }

    #[test]
    fn locks_are_respected_unless_ours_and_unclaimed() {
        let repo = init_repo_with_branch("master");
        let ours = add_worktree(&repo, ".worktrees/task/7", "task/7-x");
        let claimed = add_worktree(&repo, ".worktrees/task/8", "task/8-y");
        let foreign = add_worktree(&repo, ".worktrees/task/9", "task/9-z");
        let lock = |p: &PathBuf, reason: &str| {
            let p = p.to_str().unwrap();
            run_in(&repo.path, &["worktree", "lock", "--reason", reason, p]).unwrap();
        };
        lock(&ours, OUR_LOCK);
        lock(&claimed, OUR_LOCK);
        lock(&foreign, "pinned by a human");
        let (items, skipped) = plan(&repo, &HashSet::from(["8".to_string()]), &[]);
        assert!(
            items.iter().any(|i| i.label.ends_with("task/7")),
            "a stale agentflare lock is removable: {skipped:?}"
        );
        assert_eq!(reason_for(&skipped, "task/8"), "claimed by a live session");
        assert_eq!(reason_for(&skipped, "task/9"), "locked: pinned by a human");
    }

    // Review Focus 2: registered worktree whose directory is gone.
    #[test]
    fn missing_worktree_directory_is_skipped_not_fatal() {
        let repo = init_repo_with_branch("master");
        let wt = add_worktree(&repo, ".worktrees/task/7", "task/7-x");
        add_worktree(&repo, ".worktrees/task/8", "task/8-y");
        std::fs::remove_dir_all(&wt).unwrap();
        let (items, skipped) = plan(&repo, &HashSet::new(), &[]);
        assert_eq!(
            reason_for(&skipped, "task/7"),
            "worktree directory is missing (run `git worktree prune`)"
        );
        assert!(
            items.iter().any(|i| i.label.ends_with("task/8")),
            "the scan continues past it"
        );
    }

    #[test]
    fn unmerged_worktree_branch_is_skipped_with_the_branch_reason() {
        let repo = init_repo_with_branch("master");
        let wt = add_worktree(&repo, ".worktrees/task/7", "task/7-x");
        std::fs::write(wt.join("new.txt"), "work").unwrap();
        run_in(&wt, &["add", "new.txt"]).unwrap();
        run_in(&wt, &["commit", "-m", "real work"]).unwrap();
        let (items, skipped) = plan(&repo, &HashSet::new(), &[]);
        assert!(items.is_empty(), "{items:?}");
        assert!(
            reason_for(&skipped, "task/7").starts_with("not merged"),
            "{skipped:?}"
        );
    }

    // Review finding 1: run from a linked worktree, the main checkout's
    // branch must stay protected.
    #[test]
    fn main_checkout_branch_is_never_a_candidate_from_a_linked_worktree() {
        let repo = init_repo_with_branch("master");
        let wt = add_worktree(&repo, ".worktrees/task/7", "task/7-x");
        run_in(&repo.path, &["switch", "-c", "feature-x"]).unwrap();
        let opts = CleanOptions {
            branches: true,
            worktrees: true,
            ..Default::default()
        };
        let (claimed, states) = (HashSet::new(), HashMap::new());
        let input = ScanInput {
            repo_root: Some(&wt),
            scan_root: &wt,
            opts: &opts,
            prs: &NoPrLookup,
            pr_lookup_available: true,
            claimed_items: &claimed,
            item_states: &states,
            live: &[],
        };
        let branches = classify_local(&input, &wt, "master");
        let (items, _) = candidates(&input, &wt, branches);
        assert!(
            !items
                .iter()
                .any(|i| i.branch.as_deref() == Some("feature-x")),
            "{items:?}"
        );
    }

    // Review minor, re-graded: `status.showUntrackedFiles=no` must not hide
    // untracked work from the cleanliness check.
    #[test]
    fn untracked_files_block_removal_even_when_status_hides_them() {
        let repo = init_repo_with_branch("master");
        let wt = add_worktree(&repo, ".worktrees/task/7", "task/7-x");
        run_in(&repo.path, &["config", "status.showUntrackedFiles", "no"]).unwrap();
        std::fs::write(wt.join("notes.txt"), "unsaved work").unwrap();
        let (items, skipped) = plan(&repo, &HashSet::new(), &[]);
        assert!(items.iter().all(|i| i.kind != Kind::Worktree), "{items:?}");
        assert_eq!(reason_for(&skipped, "task/7"), "uncommitted changes");
    }
}
