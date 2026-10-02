//! Applies selected plan items: every item is re-validated right before it
//! is touched, and heavy directories are parked (renamed aside) so the
//! caller can unlink them separately with [`purge`].

use std::path::{Path, PathBuf};

use super::{Item, Kind, artifacts, par_map, worktrees};
use crate::shell::{run_in, run_in_ok};
use crate::worktree::{delete_parked, gc_orphans, park_dir};
use flare_process::cwd::LiveProc;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Outcome {
    pub id: String,
    pub ok: bool,
    pub detail: String,
}

/// Enough to undo a deletion: `git branch <name> <sha>`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RestoreEntry {
    pub kind: Kind,
    pub name: String,
    pub sha: String,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Default, serde::Serialize)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
    pub freed_bytes: u64,
    pub restore: Vec<RestoreEntry>,
    /// Trees already moved aside and awaiting [`purge`].
    #[serde(skip)]
    pub parked: Vec<PathBuf>,
}

impl Report {
    #[must_use]
    pub fn failed(&self) -> usize {
        self.outcomes.iter().filter(|o| !o.ok).count()
    }
}

/// Compare-and-delete: refuses when the branch no longer points at `sha`.
fn delete_branch(repo_root: &Path, name: &str, sha: &str) -> Result<(), String> {
    let full = format!("refs/heads/{name}");
    if run_in(repo_root, &["rev-parse", "--verify", "--quiet", &full]).as_deref() != Ok(sha) {
        return Err("branch moved since the plan; left untouched".into());
    }
    run_in(repo_root, &["update-ref", "-d", &full, sha])?;
    // The upstream/merge settings would otherwise outlive the branch.
    let _ = run_in(
        repo_root,
        &["config", "--remove-section", &format!("branch.{name}")],
    );
    Ok(())
}

/// `path` resolves to somewhere strictly inside `root`.
fn inside(root: &Path, path: &Path) -> bool {
    matches!(
        (root.canonicalize(), path.canonicalize()),
        (Ok(r), Ok(p)) if p != r && p.starts_with(&r)
    )
}

fn has_tracked_files(dir_parent: &Path, name: &str) -> bool {
    run_in_ok(dir_parent, &["rev-parse", "--is-inside-work-tree"])
        && !run_in(dir_parent, &["ls-files", "--", name]).is_ok_and(|o| o.is_empty())
}

struct Ctx<'a> {
    repo_root: Option<&'a Path>,
    scan_root: &'a Path,
    live: Vec<LiveProc>,
    /// The repo's own trash, when it already keeps a `.worktrees` directory.
    trash: Option<PathBuf>,
}

fn remove_worktree(
    ctx: &Ctx,
    item: &Item,
    path: &Path,
    report: &mut Report,
) -> Result<String, String> {
    let repo = ctx.repo_root.ok_or("not a git repository")?;
    let (name, sha) = (
        item.branch.as_deref().ok_or("item has no branch")?,
        item.sha.as_deref().ok_or("item has no sha")?,
    );
    if !run_in(path, &["status", "--porcelain"])?.is_empty() {
        return Err("uncommitted changes appeared since the plan".into());
    }
    if let Some(p) = worktrees::occupant(path, &ctx.live) {
        return Err(format!("live process: {} (pid {})", p.name, p.pid));
    }
    let full = format!("refs/heads/{name}");
    if run_in(repo, &["rev-parse", "--verify", "--quiet", &full]).as_deref() != Ok(sha) {
        return Err("branch moved since the plan; left untouched".into());
    }
    // Park the heavy ignored directories first, so git only has source
    // files left to delete. The tree is clean, so anything matched here is
    // either tracked (kept) or ignored build output.
    for (dir, _) in artifacts::find_dirs(path, &[]) {
        let rel = dir.strip_prefix(path).unwrap_or(&dir).to_string_lossy();
        if run_in(path, &["ls-files", "--", &rel]).is_ok_and(|o| o.is_empty())
            && let Ok(parked) = park_dir(&dir, &item.label, ctx.trash.as_deref())
        {
            report.parked.push(parked);
        }
    }
    let p = path.to_string_lossy();
    let _ = run_in(repo, &["worktree", "unlock", &p]);
    // No --force: git's own dirty-tree refusal stays as a second guard.
    run_in(repo, &["worktree", "remove", &p])?;
    delete_branch(repo, name, sha)?;
    report.restore.push(RestoreEntry {
        kind: Kind::Worktree,
        name: name.into(),
        sha: sha.into(),
        path: Some(path.to_path_buf()),
    });
    Ok("worktree and branch removed".into())
}

fn remove_artifact(
    ctx: &Ctx,
    item: &Item,
    path: &Path,
    report: &mut Report,
) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_dir() {
        return Err("no longer a directory".into());
    }
    if !inside(ctx.scan_root, path) {
        return Err("path is outside the scan root".into());
    }
    let (parent, name) = (
        path.parent().ok_or("path has no parent")?,
        path.file_name()
            .and_then(|n| n.to_str())
            .ok_or("path has no name")?,
    );
    if has_tracked_files(parent, name) {
        return Err("contains tracked files".into());
    }
    if let Some(p) = worktrees::occupant(path, &ctx.live) {
        return Err(format!("in use: {} (pid {})", p.name, p.pid));
    }
    report
        .parked
        .push(park_dir(path, &item.label, ctx.trash.as_deref())?);
    Ok("removed".into())
}

fn apply_one(ctx: &Ctx, item: &Item, report: &mut Report) -> Result<String, String> {
    let path = || item.path.as_deref().ok_or("item has no path");
    match item.kind {
        Kind::Branch => {
            let repo = ctx.repo_root.ok_or("not a git repository")?;
            let (name, sha) = (
                item.branch.as_deref().ok_or("item has no branch")?,
                item.sha.as_deref().ok_or("item has no sha")?,
            );
            delete_branch(repo, name, sha)?;
            report.restore.push(RestoreEntry {
                kind: Kind::Branch,
                name: name.into(),
                sha: sha.into(),
                path: None,
            });
            Ok(format!("deleted (was {})", &sha[..sha.len().min(7)]))
        }
        Kind::Worktree => remove_worktree(ctx, item, path()?, report),
        Kind::Orphan => {
            let repo = ctx.repo_root.ok_or("not a git repository")?;
            let name = item
                .id
                .strip_prefix("orphan:")
                .ok_or("malformed orphan id")?;
            // Snapshots first, then removes; reports its own stderr detail.
            if gc_orphans(repo, &[name.to_string()])
                .iter()
                .any(|n| n == name)
            {
                Ok("orphan removed (snapshot taken)".into())
            } else {
                Err("could not remove orphan".into())
            }
        }
        Kind::Artifact => remove_artifact(ctx, item, path()?, report),
        Kind::Remote => Err("remote branches are deleted by the caller".into()),
    }
}

/// Applies local items (Branch, Worktree, Orphan, Artifact) one by one; a
/// failure is recorded and the rest still run. `Kind::Remote` needs the
/// network and is the caller's job.
pub fn apply(
    repo_root: Option<&Path>,
    scan_root: &Path,
    items: &[Item],
    on_progress: &mut dyn FnMut(&Outcome),
) -> Report {
    let ctx = Ctx {
        repo_root,
        scan_root,
        live: flare_process::cwd::live_procs(),
        trash: repo_root
            .map(|r| r.join(".worktrees"))
            .filter(|w| w.is_dir())
            .map(|w| w.join(".trash")),
    };
    let mut report = Report::default();
    for item in items {
        let (ok, detail) = match apply_one(&ctx, item, &mut report) {
            Ok(detail) => {
                report.freed_bytes += item.size_bytes;
                (true, detail)
            }
            Err(detail) => (false, detail),
        };
        let outcome = Outcome {
            id: item.id.clone(),
            ok,
            detail,
        };
        on_progress(&outcome);
        report.outcomes.push(outcome);
    }
    report
}

/// Unlinks parked trees in parallel. Best-effort: a leftover is swept by a
/// later removal.
pub fn purge(parked: &[PathBuf]) {
    par_map(parked, |p| delete_parked(p));
    let mut trashes: Vec<&Path> = parked.iter().filter_map(|p| p.parent()).collect();
    trashes.sort_unstable();
    trashes.dedup();
    for trash in trashes {
        let _ = std::fs::remove_dir(trash); // succeeds only when empty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::tests::{git_opts, scan_repo};
    use crate::clean::{Item, Kind};
    use crate::shell::test_support::init_repo_with_branch;
    use crate::shell::{run_in, run_in_ok};

    fn has_branch(repo: &std::path::Path, name: &str) -> bool {
        let full = format!("refs/heads/{name}");
        run_in_ok(repo, &["rev-parse", "--verify", "--quiet", &full])
    }

    #[test]
    fn apply_deletes_branch_and_records_restore_entry() {
        let repo = init_repo_with_branch("master");
        run_in(&repo.path, &["branch", "done"]).unwrap();
        let plan = scan_repo(&repo.path, &git_opts());
        let mut seen = Vec::new();
        let report = apply(Some(&repo.path), &repo.path, &plan.items, &mut |o| {
            seen.push(o.id.clone());
        });
        assert_eq!(report.failed(), 0, "{:?}", report.outcomes);
        assert_eq!(seen, ["branch:done"]);
        assert!(!has_branch(&repo.path, "done"));
        assert_eq!(report.restore[0].name, "done");
        assert_eq!(Some(&report.restore[0].sha), plan.items[0].sha.as_ref());
    }

    #[test]
    fn branch_that_moved_since_the_plan_is_not_deleted() {
        let repo = init_repo_with_branch("master");
        run_in(&repo.path, &["branch", "done"]).unwrap();
        let plan = scan_repo(&repo.path, &git_opts());
        run_in(&repo.path, &["switch", "done"]).unwrap();
        run_in(&repo.path, &["commit", "--allow-empty", "-m", "new work"]).unwrap();
        run_in(&repo.path, &["switch", "master"]).unwrap();
        let report = apply(Some(&repo.path), &repo.path, &plan.items, &mut |_| {});
        assert_eq!(report.failed(), 1);
        assert!(
            report.outcomes[0].detail.contains("moved"),
            "{:?}",
            report.outcomes
        );
        assert!(has_branch(&repo.path, "done"));
        assert!(report.restore.is_empty());
    }

    #[test]
    fn apply_removes_worktree_with_its_branch_and_parks_artifacts() {
        let repo = init_repo_with_branch("master");
        std::fs::write(repo.path.join(".gitignore"), "target/\n.worktrees/\n").unwrap();
        std::fs::write(repo.path.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        run_in(&repo.path, &["add", "."]).unwrap();
        run_in(&repo.path, &["commit", "-m", "base"]).unwrap();
        let wt = repo.path.join(".worktrees/task/7");
        let p = wt.to_str().unwrap();
        run_in(&repo.path, &["worktree", "add", "-b", "task/7-x", p]).unwrap();
        std::fs::create_dir_all(wt.join("target/debug")).unwrap();
        std::fs::write(wt.join("target/debug/big.o"), vec![0u8; 4096]).unwrap();

        let plan = scan_repo(&repo.path, &git_opts());
        let report = apply(Some(&repo.path), &repo.path, &plan.items, &mut |_| {});
        assert_eq!(report.failed(), 0, "{:?}", report.outcomes);
        assert!(!wt.exists());
        assert!(!has_branch(&repo.path, "task/7-x"));
        assert!(report.freed_bytes >= 4096);
        assert_eq!(
            report.parked.len(),
            1,
            "the worktree's target/ is parked, not unlinked inline"
        );
        assert!(report.parked[0].exists());
        purge(&report.parked);
        assert!(!report.parked[0].exists());
    }

    #[test]
    fn worktree_dirtied_after_the_plan_is_left_alone() {
        let repo = init_repo_with_branch("master");
        let wt = repo.path.join(".worktrees/task/7");
        let p = wt.to_str().unwrap();
        run_in(&repo.path, &["worktree", "add", "-b", "task/7-x", p]).unwrap();
        let plan = scan_repo(&repo.path, &git_opts());
        std::fs::write(wt.join("wip.txt"), "late edit").unwrap();
        let report = apply(Some(&repo.path), &repo.path, &plan.items, &mut |_| {});
        assert_eq!(report.failed(), 1);
        assert!(wt.join("wip.txt").exists());
        assert!(has_branch(&repo.path, "task/7-x"));
    }

    #[test]
    fn artifact_is_parked_and_tracked_files_block_it() {
        let repo = init_repo_with_branch("master");
        let r = &repo.path;
        std::fs::write(r.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(r.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
        std::fs::create_dir_all(r.join("target")).unwrap();
        std::fs::write(r.join("target/a.o"), b"obj").unwrap();
        std::fs::create_dir_all(r.join("docs")).unwrap();
        std::fs::write(r.join("docs/readme.md"), b"tracked").unwrap();
        run_in(r, &["add", "."]).unwrap();
        run_in(r, &["commit", "-m", "base"]).unwrap();
        let artifact = |name: &str| Item {
            id: format!("artifact:{name}"),
            kind: Kind::Artifact,
            label: name.into(),
            path: Some(r.join(name)),
            branch: None,
            sha: None,
            size_bytes: 3,
            reason: String::new(),
        };
        let report = apply(
            Some(r),
            r,
            &[artifact("target"), artifact("docs")],
            &mut |_| {},
        );
        assert!(report.outcomes[0].ok, "{:?}", report.outcomes);
        assert!(!r.join("target").exists());
        assert!(!report.outcomes[1].ok);
        assert!(r.join("docs/readme.md").exists());
        assert_eq!(report.freed_bytes, 3);
        purge(&report.parked);
    }

    #[test]
    fn artifact_outside_the_scan_root_is_refused() {
        let inside = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let victim = outside.path().join("target");
        std::fs::create_dir_all(&victim).unwrap();
        let item = Item {
            id: "artifact:target".into(),
            kind: Kind::Artifact,
            label: "target".into(),
            path: Some(victim.clone()),
            branch: None,
            sha: None,
            size_bytes: 0,
            reason: String::new(),
        };
        let report = apply(None, inside.path(), &[item], &mut |_| {});
        assert_eq!(report.failed(), 1);
        assert!(victim.exists());
    }
}
