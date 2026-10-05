//! `agentflare clean`: plan-then-confirm cleanup of merged branches,
//! worktrees and build artifacts. The engine is `flare_git_core::clean`;
//! this file owns the arguments, the GitHub lookup, the terminal flow, the
//! remote push and the restore log.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use flare_git_core::clean::{
    self, CleanOptions, Item, Kind, NoPrLookup, Outcome, Plan, PrInfo, PrLookup, PrState,
    RestoreEntry, ScanInput,
};
use flare_git_core::shell::{run_in, run_in_ok};

use crate::ui;

#[derive(clap::Args, Default)]
pub struct CleanArgs {
    /// Directory to scan (defaults to the repository root).
    pub path: Option<PathBuf>,
    /// Only merged local branches.
    #[arg(long)]
    pub branches: bool,
    /// Only merged worktrees and orphaned worktree directories.
    #[arg(long)]
    pub worktrees: bool,
    /// Build/cache directories, recursively. Limit the kinds with
    /// `--artifacts=rust,node,python,...`.
    #[arg(
        long,
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "all",
        value_delimiter = ','
    )]
    pub artifacts: Option<Vec<String>>,
    /// Also delete merged branches on origin.
    #[arg(long)]
    pub remote: bool,
    /// Branches, worktrees, artifacts and remote branches.
    #[arg(long)]
    pub all: bool,
    /// Only artifacts untouched for at least this long (12h, 7d, 2w, 3M).
    #[arg(long, value_name = "AGE")]
    pub older_than: Option<String>,
    /// Hide artifacts smaller than this (500K, 50M, 2G).
    #[arg(long, value_name = "SIZE")]
    pub min_size: Option<String>,
    /// Keep only items whose branch name or path matches this glob (`*`
    /// also spans `/`). Repeatable.
    #[arg(long, value_name = "GLOB")]
    pub only: Vec<String>,
    /// Drop items whose branch name or path matches this glob. Repeatable.
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Print the plan and exit; never prompts, never deletes.
    #[arg(long)]
    pub dry_run: bool,
    /// Apply without prompting.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

fn parse_duration(s: &str) -> Result<Duration, String> {
    let bad = || format!("invalid duration '{s}' (use e.g. 12h, 7d, 2w, 3M)");
    let split = s.len().checked_sub(1).filter(|i| s.is_char_boundary(*i));
    let (num, unit) = s.split_at(split.ok_or_else(bad)?);
    let unit_secs = match unit {
        "h" => 3600,
        "d" => 86_400,
        "w" => 7 * 86_400,
        "M" => 30 * 86_400,
        _ => return Err(bad()),
    };
    let n: u64 = num.parse().map_err(|_| bad())?;
    n.checked_mul(unit_secs)
        .map(Duration::from_secs)
        .ok_or_else(bad)
}

fn parse_size(s: &str) -> Result<u64, String> {
    let bad = || format!("invalid size '{s}' (use e.g. 500K, 50M, 2G)");
    let (num, shift) = match s.chars().last() {
        Some('K' | 'k') => (&s[..s.len() - 1], 10),
        Some('M' | 'm') => (&s[..s.len() - 1], 20),
        Some('G' | 'g') => (&s[..s.len() - 1], 30),
        _ => (s, 0),
    };
    let n: u64 = num.parse().map_err(|_| bad())?;
    n.checked_mul(1 << shift).ok_or_else(bad)
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// With no category flag the run is git cleanup; naming any category runs
/// only the named ones; `--all` runs everything.
fn options(args: &CleanArgs) -> Result<CleanOptions, String> {
    // A bare `--artifacts` arrives as ["all"]: every kind.
    let kinds: Option<Vec<String>> = args
        .artifacts
        .as_ref()
        .map(|k| k.iter().filter(|k| *k != "all").cloned().collect());
    let known = clean::artifacts::kinds();
    if let Some(bad) = kinds
        .iter()
        .flatten()
        .find(|k| !known.contains(&k.as_str()))
    {
        return Err(format!(
            "unknown artifact kind '{bad}' (known: {})",
            known.join(", ")
        ));
    }
    let named = args.branches || args.worktrees || args.remote || args.artifacts.is_some();
    Ok(CleanOptions {
        branches: args.all || args.branches || !named,
        worktrees: args.all || args.worktrees || !named,
        artifacts: if args.all {
            Some(kinds.unwrap_or_default())
        } else {
            kinds
        },
        remote: args.all || args.remote,
        older_than: args.older_than.as_deref().map(parse_duration).transpose()?,
        min_size: args
            .min_size
            .as_deref()
            .map(parse_size)
            .transpose()?
            .unwrap_or(0),
        only: args.only.clone(),
        exclude: args.exclude.clone(),
    })
}

/// PR lookup over the GitHub REST client. For a merged PR whose head commit
/// is not local (GitHub deleted the branch), fetches `refs/pull/<n>/head`
/// so the engine can check the local tip is contained in it.
struct GithubPrs {
    client: crate::github::Client,
    repo: crate::github::RepoId,
    repo_root: PathBuf,
}

impl PrLookup for GithubPrs {
    fn for_branch(&self, branch: &str) -> Option<PrInfo> {
        let pr = crate::github::pulls::find_existing(&self.client, &self.repo, branch).ok()??;
        let head_sha = pr.head.as_ref().map(|h| h.sha.clone()).unwrap_or_default();
        let state = if pr.merged_at.is_some() {
            PrState::Merged
        } else if pr.state == "open" {
            PrState::Open
        } else {
            PrState::Closed
        };
        let head_commit = format!("{head_sha}^{{commit}}");
        if state == PrState::Merged
            && !run_in_ok(&self.repo_root, &["cat-file", "-e", &head_commit])
        {
            let pull_ref = format!("refs/pull/{}/head", pr.number);
            let _ = run_in(&self.repo_root, &["fetch", "--quiet", "origin", &pull_ref]);
        }
        Some(PrInfo {
            number: pr.number,
            state,
            head_sha,
        })
    }
}

fn github_prs(repo_root: &Path) -> Option<GithubPrs> {
    let repo = crate::github::bridge::config::resolve_project_repo(repo_root).ok()??;
    let client = crate::github::Client::new().ok()?;
    Some(GithubPrs {
        client,
        repo,
        repo_root: repo_root.to_path_buf(),
    })
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Branch => "branch",
        Kind::Worktree => "worktree",
        Kind::Orphan => "orphan",
        Kind::Artifact => "artifact",
        Kind::DepsStore => "deps store",
        Kind::Remote => "remote",
    }
}

/// `(row, hint)` for one item: kind, label and size, then why it is listed.
fn row(item: &Item) -> (String, String) {
    let size = match item.size_bytes {
        0 => String::new(),
        n => human_size(n),
    };
    (
        format!("{:<9} {:<44} {:>9}", kind_word(item.kind), item.label, size),
        item.reason.clone(),
    )
}

fn total(items: &[&Item]) -> String {
    let bytes = items.iter().map(|i| i.size_bytes).sum();
    format!("{} item(s) · {}", items.len(), human_size(bytes))
}

fn print_notes_and_skipped(plan: &Plan) {
    for note in &plan.notes {
        ui::warning(note);
    }
    if !plan.skipped.is_empty() {
        let body: Vec<String> = plan
            .skipped
            .iter()
            .map(|s| format!("{:<44}  {}", s.label, s.reason))
            .collect();
        ui::note(
            &format!("Skipped · {}", plan.skipped.len()),
            &body.join("\n"),
        );
    }
}

fn print_plan(plan: &Plan) {
    let all: Vec<&Item> = plan.items.iter().collect();
    let body: Vec<String> = all
        .iter()
        .map(|i| {
            let (line, why) = row(i);
            format!("{line}  {why}")
        })
        .collect();
    ui::note(&format!("Would remove · {}", total(&all)), &body.join("\n"));
    print_notes_and_skipped(plan);
}

/// One all-or-nothing push: every delete is guarded by a lease on the SHA
/// seen at plan time, so a remote branch that moved since is refused.
fn remote_delete_args(items: &[&Item]) -> Vec<String> {
    let branch = |i: &Item| i.branch.clone().unwrap_or_default();
    let leases = items.iter().map(|i| {
        format!(
            "--force-with-lease=refs/heads/{}:{}",
            branch(i),
            i.sha.as_deref().unwrap_or_default()
        )
    });
    let deletes = items.iter().map(|i| format!(":refs/heads/{}", branch(i)));
    // `--atomic`: one refused lease refuses the whole push, so the single
    // exit status is the truth for every branch.
    ["push", "--atomic", "origin"]
        .map(String::from)
        .into_iter()
        .chain(leases)
        .chain(deletes)
        .collect()
}

fn delete_remote(repo_root: &Path, items: &[&Item]) -> (Vec<Outcome>, Vec<RestoreEntry>) {
    if items.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let args = remote_delete_args(items);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = run_in(repo_root, &argv);
    let outcomes = items
        .iter()
        .map(|i| Outcome {
            id: i.id.clone(),
            ok: result.is_ok(),
            detail: match &result {
                Ok(_) => "deleted on origin".into(),
                Err(e) => e.clone(),
            },
        })
        .collect();
    let restore = items
        .iter()
        .filter(|_| result.is_ok())
        .map(|i| RestoreEntry {
            kind: Kind::Remote,
            name: i.branch.clone().unwrap_or_default(),
            sha: i.sha.clone().unwrap_or_default(),
            path: None,
        })
        .collect();
    (outcomes, restore)
}

/// Writes what was deleted, with SHAs, so a branch can be recreated with
/// `git branch <name> <sha>`.
fn write_restore_log(entries: &[RestoreEntry]) -> Option<PathBuf> {
    if entries.is_empty() {
        return None;
    }
    let dir = crate::paths::agentflare_dir().join("clean");
    let name = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S%.3f");
    let path = dir.join(format!("{name}.json"));
    let written = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&path, serde_json::to_vec_pretty(entries)?));
    match written {
        Ok(()) => Some(path),
        Err(e) => {
            ui::warning(&format!(
                "could not write the restore log {}: {e}",
                path.display()
            ));
            None
        }
    }
}

fn fail(message: &str) -> ! {
    ui::error(&format!("agentflare clean: {message}"));
    std::process::exit(2);
}

/// Scan root and, when it is inside one, the git repository it belongs to.
fn roots(path: Option<&Path>) -> Result<(PathBuf, Option<PathBuf>), String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let start = path.unwrap_or(&cwd);
    // Always the main checkout, even when run from a linked worktree: that
    // is where `.worktrees`, item state and claims are keyed.
    let repo_root = flare_git_core::branch::repo_toplevel(start)
        .map(|top| flare_git_core::branch::main_worktree_root(&top).unwrap_or(top));
    let scan_root = match (path, &repo_root) {
        (Some(p), _) => p.to_path_buf(),
        (None, Some(r)) => r.clone(),
        (None, None) => cwd,
    };
    if !scan_root.is_dir() {
        return Err(format!("{}: not a directory", scan_root.display()));
    }
    let scan_root = agentflare_config::paths::canonical(&scan_root);
    let home = agentflare_config::paths::canonical(&crate::paths::home());
    if scan_root.parent().is_none() || home == scan_root {
        return Err(
            "refusing to scan the filesystem root or your home directory; pass a project directory"
                .into(),
        );
    }
    Ok((scan_root, repo_root))
}

fn scan(repo_root: Option<&Path>, scan_root: &Path, opts: &CleanOptions) -> Plan {
    let git = opts.branches || opts.worktrees || opts.remote;
    let prs = repo_root.filter(|_| git).and_then(|r| {
        // Refresh `origin/*` so "merged" is judged against the real remote.
        let _ = run_in(r, &["fetch", "--prune", "--quiet", "origin"]);
        github_prs(r)
    });
    let (claimed, states): (HashSet<String>, HashMap<String, String>) = match repo_root {
        Some(r) => (
            super::git::claimed_sequence_ids(r),
            super::git::item_state_groups(r),
        ),
        None => Default::default(),
    };
    let live = flare_process::cwd::live_procs();
    clean::scan(&ScanInput {
        repo_root,
        scan_root,
        opts,
        prs: prs
            .as_ref()
            .map_or(&NoPrLookup as &dyn PrLookup, |p| p as &dyn PrLookup),
        pr_lookup_available: prs.is_some(),
        claimed_items: &claimed,
        item_states: &states,
        live: &live,
    })
}

/// The interactive picker and confirmation. `None` means nothing to do.
fn choose(plan: &Plan) -> Option<Vec<&Item>> {
    print_notes_and_skipped(plan);
    let all: Vec<&Item> = plan.items.iter().collect();
    let rows: Vec<(String, String, String)> = all
        .iter()
        .map(|i| {
            let (line, why) = row(i);
            (i.id.clone(), line, why)
        })
        .collect();
    let ids: Vec<String> = all.iter().map(|i| i.id.clone()).collect();
    let prompt = format!("Select what to remove · {}", total(&all));
    let Some(picked) = ui::multiselect(&prompt, &rows, &ids) else {
        ui::outro("Cancelled. Nothing was deleted.");
        return None;
    };
    let chosen: Vec<&Item> = all.into_iter().filter(|i| picked.contains(&i.id)).collect();
    if chosen.is_empty() || !ui::confirm(&format!("Remove {}?", total(&chosen)), false) {
        ui::outro("Nothing was deleted.");
        return None;
    }
    Some(chosen)
}

pub fn run(args: CleanArgs) {
    let opts = options(&args).unwrap_or_else(|e| fail(&e));
    let (scan_root, repo_root) = roots(args.path.as_deref()).unwrap_or_else(|e| fail(&e));
    let repo_root = repo_root.as_deref();
    // `--json` keeps stdout to the JSON document alone.
    let human = !args.json;
    let interactive = human && ui::interactive();
    if interactive {
        ui::intro("agentflare clean");
    }

    let plan = if human {
        ui::with_spinner("Scanning…", "Scan complete", || {
            scan(repo_root, &scan_root, &opts)
        })
    } else {
        scan(repo_root, &scan_root, &opts)
    };

    let apply_now = args.yes && !args.dry_run;
    if !human && !apply_now {
        println!("{}", serde_json::json!(plan));
        return;
    }
    if plan.items.is_empty() {
        if human {
            print_notes_and_skipped(&plan);
            ui::success("Nothing to clean");
        } else {
            println!(
                "{}",
                serde_json::json!({ "items": [], "skipped": plan.skipped, "notes": plan.notes, "outcomes": [] })
            );
        }
        return;
    }

    let chosen: Vec<&Item> = if apply_now {
        plan.items.iter().collect()
    } else if args.dry_run || !interactive {
        print_plan(&plan);
        if !args.dry_run {
            ui::info(
                "Nothing was deleted. Re-run with -y to apply, or in a terminal to choose items.",
            );
        }
        return;
    } else {
        match choose(&plan) {
            Some(chosen) => chosen,
            None => return,
        }
    };

    let (remote, local): (Vec<&Item>, Vec<&Item>) =
        chosen.into_iter().partition(|i| i.kind == Kind::Remote);
    let local: Vec<Item> = local.into_iter().cloned().collect();
    let progress = human.then(|| ui::Progress::start(local.len() as u64, "Removing"));
    // Read again, not reused from the scan: the picker may have sat open.
    let claimed = repo_root
        .map(super::git::claimed_sequence_ids)
        .unwrap_or_default();
    let mut report = clean::apply(repo_root, &scan_root, &local, &claimed, &mut |o| {
        if let Some(p) = &progress {
            p.inc(&o.id);
        }
    });
    if let Some(p) = progress {
        p.stop("Removed");
    }
    if human && !report.parked.is_empty() {
        ui::with_spinner("Freeing disk space…", "Disk space freed", || {
            clean::purge(&report.parked);
        });
    } else {
        clean::purge(&report.parked);
    }
    if let Some(r) = repo_root {
        let (outcomes, restore) = delete_remote(r, &remote);
        report.outcomes.extend(outcomes);
        report.restore.extend(restore);
    }
    let log = write_restore_log(&report.restore);

    if human {
        for o in report.outcomes.iter().filter(|o| !o.ok) {
            ui::error(&format!("{}: {}", o.id, o.detail));
        }
        let done = report.outcomes.len() - report.failed();
        let mut summary = format!(
            "Removed {done} item(s) · freed {}",
            human_size(report.freed_bytes)
        );
        if let Some(path) = &log {
            summary.push_str(&format!(" · restore log {}", path.display()));
        }
        if interactive {
            ui::outro(&summary);
        } else {
            ui::success(&summary);
        }
    } else {
        println!(
            "{}",
            serde_json::json!({
                "items": plan.items,
                "skipped": plan.skipped,
                "notes": plan.notes,
                "outcomes": report.outcomes,
                "freed_bytes": report.freed_bytes,
                "restore_log": log,
            })
        );
    }
    if report.failed() > 0 {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_durations() {
        assert_eq!(
            parse_duration("12h").unwrap(),
            Duration::from_secs(12 * 3600)
        );
        assert_eq!(
            parse_duration("7d").unwrap(),
            Duration::from_secs(7 * 86_400)
        );
        assert_eq!(
            parse_duration("2w").unwrap(),
            Duration::from_secs(14 * 86_400)
        );
        assert_eq!(
            parse_duration("3M").unwrap(),
            Duration::from_secs(90 * 86_400)
        );
        for bad in ["soon", "", "d", "7", "7x", "-1d"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("500K").unwrap(), 500 * 1024);
        assert_eq!(parse_size("50M").unwrap(), 50 * 1024 * 1024);
        assert_eq!(parse_size("2g").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("123").unwrap(), 123);
        for bad in ["big", "", "M", "1.5G"] {
            assert!(parse_size(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn human_size_picks_a_sensible_unit() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn no_category_flag_means_git_cleanup_only() {
        let o = options(&CleanArgs::default()).unwrap();
        assert!(o.branches && o.worktrees && o.artifacts.is_none() && !o.remote);
    }

    #[test]
    fn a_category_flag_narrows_the_run() {
        let o = options(&CleanArgs {
            branches: true,
            ..CleanArgs::default()
        })
        .unwrap();
        assert!(o.branches && !o.worktrees);
        let o = options(&CleanArgs {
            artifacts: Some(vec!["rust".into()]),
            ..CleanArgs::default()
        })
        .unwrap();
        assert!(!o.branches && !o.worktrees && !o.remote);
        assert_eq!(o.artifacts, Some(vec!["rust".to_string()]));
    }

    #[test]
    fn bare_artifacts_flag_means_every_kind() {
        // clap hands a bare `--artifacts` over as ["all"].
        let o = options(&CleanArgs {
            artifacts: Some(vec!["all".into()]),
            ..CleanArgs::default()
        })
        .unwrap();
        assert_eq!(o.artifacts, Some(Vec::new()));
    }

    #[test]
    fn all_enables_everything() {
        let o = options(&CleanArgs {
            all: true,
            ..CleanArgs::default()
        })
        .unwrap();
        assert!(o.branches && o.worktrees && o.remote);
        assert_eq!(o.artifacts, Some(Vec::new()));
    }

    #[test]
    fn unknown_artifact_kind_is_an_error() {
        let err = options(&CleanArgs {
            artifacts: Some(vec!["cobol".into()]),
            ..CleanArgs::default()
        })
        .unwrap_err();
        assert!(err.contains("cobol") && err.contains("rust"), "{err}");
    }

    #[test]
    fn remote_delete_args_lease_every_branch_on_its_planned_sha() {
        let item = |b: &str, sha: &str| Item {
            id: format!("remote:{b}"),
            kind: Kind::Remote,
            label: format!("origin/{b}"),
            path: None,
            branch: Some(b.into()),
            sha: Some(sha.into()),
            size_bytes: 0,
            reason: String::new(),
        };
        let (a, b) = (item("a", "111"), item("b", "222"));
        assert_eq!(
            remote_delete_args(&[&a, &b]),
            [
                "push",
                "--atomic",
                "origin",
                "--force-with-lease=refs/heads/a:111",
                "--force-with-lease=refs/heads/b:222",
                ":refs/heads/a",
                ":refs/heads/b",
            ]
        );
    }

    // Review finding 7: one stale lease must not leave the other branch
    // deleted while the run reports everything as failed.
    #[test]
    fn remote_delete_is_all_or_nothing_when_one_lease_is_stale() {
        let git = |dir: &Path, args: &[&str]| run_in(dir, args).unwrap();
        let remote = tempfile::TempDir::new().unwrap();
        git(remote.path(), &["init", "--bare", "-b", "master"]);
        let local = tempfile::TempDir::new().unwrap();
        let repo = local.path();
        git(repo, &["init", "-b", "master"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "T"]);
        git(repo, &["commit", "--allow-empty", "-m", "initial"]);
        git(
            repo,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        git(repo, &["branch", "a"]);
        git(repo, &["branch", "b"]);
        git(repo, &["push", "origin", "master", "a", "b"]);
        let tip = git(repo, &["rev-parse", "master"]);
        let item = |b: &str, sha: &str| Item {
            id: format!("remote:{b}"),
            kind: Kind::Remote,
            label: format!("origin/{b}"),
            path: None,
            branch: Some(b.into()),
            sha: Some(sha.into()),
            size_bytes: 0,
            reason: String::new(),
        };
        // `b` moved on the remote since the plan: its lease names a stale sha.
        let stale = "1111111111111111111111111111111111111111";
        let (a, b) = (item("a", &tip), item("b", stale));
        let (outcomes, restore) = delete_remote(repo, &[&a, &b]);
        assert!(outcomes.iter().all(|o| !o.ok), "{outcomes:?}");
        assert!(restore.is_empty());
        let heads = git(
            remote.path(),
            &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
        );
        assert!(heads.lines().any(|l| l == "a"), "a must survive: {heads}");

        let (outcomes, restore) = delete_remote(repo, &[&a]);
        assert!(outcomes[0].ok, "{outcomes:?}");
        assert_eq!(restore[0].name, "a");
    }
}
