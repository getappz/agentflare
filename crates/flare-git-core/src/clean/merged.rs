//! Decides whether a branch's work is already in the default branch.

use std::path::Path;

use super::{Item, Kind, PrState, ScanInput, Skipped, par_map};
use crate::branch::is_protected_branch;
use crate::shell::{run_in, run_in_ok, run_in_opt};

pub(super) enum Verdict {
    /// Safe to delete; the string says why it counts as merged.
    Merged(String),
    Skip(String),
}

pub(super) struct BranchInfo {
    pub name: String,
    pub sha: String,
    pub verdict: Verdict,
}

/// `origin/<default>` when that ref exists, else the local `<default>`.
pub(super) fn base_ref(repo_root: &Path, default: &str) -> String {
    let remote = format!("refs/remotes/origin/{default}");
    if run_in_ok(repo_root, &["rev-parse", "--verify", "--quiet", &remote]) {
        format!("origin/{default}")
    } else {
        default.to_string()
    }
}

/// `<n>` from `task/<n>` or `task/<n>-slug`.
fn task_sequence_id(branch: &str) -> Option<&str> {
    let id = branch.strip_prefix("task/")?.split('-').next()?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

/// `(short name, sha)` for every ref under `namespace`.
fn refs(repo_root: &Path, namespace: &str) -> Vec<(String, String)> {
    let format = "--format=%(refname:short) %(objectname)";
    run_in(repo_root, &["for-each-ref", format, namespace])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.rsplit_once(' '))
        .map(|(name, sha)| (name.to_string(), sha.to_string()))
        .collect()
}

fn is_ancestor(repo_root: &Path, a: &str, b: &str) -> bool {
    run_in_ok(repo_root, &["merge-base", "--is-ancestor", a, b])
}

pub(super) fn classify_local(
    input: &ScanInput,
    repo_root: &Path,
    default: &str,
) -> Vec<BranchInfo> {
    let base = base_ref(repo_root, default);
    let current = run_in_opt(repo_root, &["symbolic-ref", "--short", "HEAD"]);
    par_map(&refs(repo_root, "refs/heads"), |(name, sha)| BranchInfo {
        name: name.clone(),
        sha: sha.clone(),
        verdict: classify_one(
            input,
            repo_root,
            default,
            &base,
            current.as_deref(),
            name,
            sha,
        ),
    })
}

fn classify_one(
    input: &ScanInput,
    repo_root: &Path,
    default: &str,
    base: &str,
    current: Option<&str>,
    name: &str,
    sha: &str,
) -> Verdict {
    if is_protected_branch(name, Some(default)) {
        return Verdict::Skip("protected branch".into());
    }
    if current.map(str::trim) == Some(name) {
        return Verdict::Skip("current branch".into());
    }
    let full = format!("refs/heads/{name}");
    if is_ancestor(repo_root, &full, base) {
        // A fresh task branch looks "merged" only because work has not started.
        if let Some(id) = task_sequence_id(name)
            && let Some(state) = input.item_states.get(id)
            && state != "completed"
            && state != "cancelled"
        {
            return Verdict::Skip(format!("no commits yet; item #{id} is {state}"));
        }
        return Verdict::Merged(format!("merged into {default}"));
    }
    // `git cherry` marks a commit `+` when no patch-equivalent is upstream.
    // An unreadable answer counts as "has unmerged commits".
    let cherry = run_in(repo_root, &["cherry", base, &full]).unwrap_or_else(|_| "+".into());
    let ahead = cherry.lines().filter(|l| l.starts_with('+')).count();
    if ahead == 0 {
        return Verdict::Merged(format!("patches already in {default}"));
    }
    let Some(pr) = input.prs.for_branch(name) else {
        return Verdict::Skip(format!("not merged ({ahead} commit(s) not in {default})"));
    };
    match pr.state {
        PrState::Open => Verdict::Skip(format!("open PR #{}", pr.number)),
        PrState::Closed => Verdict::Skip(format!("PR #{} closed, not merged", pr.number)),
        PrState::Merged if sha == pr.head_sha => {
            Verdict::Merged(format!("PR #{} merged", pr.number))
        }
        PrState::Merged => {
            let head = format!("{}^{{commit}}", pr.head_sha);
            if !run_in_ok(repo_root, &["cat-file", "-e", &head]) {
                Verdict::Skip(format!("PR #{} merged, cannot verify local tip", pr.number))
            } else if is_ancestor(repo_root, sha, &pr.head_sha) {
                Verdict::Merged(format!("PR #{} merged", pr.number))
            } else {
                Verdict::Skip(format!(
                    "PR #{} merged, but local commits were not in it",
                    pr.number
                ))
            }
        }
    }
}

/// Branches on `origin` whose work is in the default branch.
pub(super) fn classify_remote(
    input: &ScanInput,
    repo_root: &Path,
    default: &str,
) -> (Vec<Item>, Vec<Skipped>) {
    let base = format!("origin/{default}");
    let (mut items, mut skipped) = (Vec::new(), Vec::new());
    for (short, sha) in refs(repo_root, "refs/remotes/origin") {
        // `origin/HEAD` prints as `origin` or `origin/HEAD` depending on git.
        let Some(name) = short.strip_prefix("origin/") else {
            continue;
        };
        if name == "HEAD" || is_protected_branch(name, Some(default)) {
            continue;
        }
        let label = format!("origin/{name}");
        let merged = if is_ancestor(repo_root, &sha, &base) {
            Ok(format!("merged into {default}"))
        } else {
            match input.prs.for_branch(name) {
                Some(pr) if pr.state == PrState::Merged && pr.head_sha == sha => {
                    Ok(format!("PR #{} merged", pr.number))
                }
                Some(pr) if pr.state == PrState::Open => Err(format!("open PR #{}", pr.number)),
                _ => Err("not merged".to_string()),
            }
        };
        match merged {
            Ok(reason) => items.push(Item {
                id: format!("remote:{name}"),
                kind: Kind::Remote,
                label,
                path: None,
                branch: Some(name.to_string()),
                sha: Some(sha),
                size_bytes: 0,
                reason,
            }),
            Err(reason) => skipped.push(Skipped { label, reason }),
        }
    }
    (items, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::{CleanOptions, NoPrLookup, PrInfo, PrLookup, PrState, ScanInput};
    use crate::shell::run_in;
    use crate::shell::test_support::{Repo, init_repo_with_branch};
    use std::collections::{HashMap, HashSet};

    struct FakePrs(HashMap<String, PrInfo>);
    impl PrLookup for FakePrs {
        fn for_branch(&self, b: &str) -> Option<PrInfo> {
            self.0.get(b).cloned()
        }
    }

    fn pr(branch: &str, number: u64, state: PrState, head_sha: &str) -> FakePrs {
        FakePrs(HashMap::from([(
            branch.to_string(),
            PrInfo {
                number,
                state,
                head_sha: head_sha.to_string(),
            },
        )]))
    }

    fn commit(repo: &Repo, file: &str, body: &str, msg: &str) -> String {
        std::fs::write(repo.path.join(file), body).unwrap();
        run_in(&repo.path, &["add", file]).unwrap();
        run_in(&repo.path, &["commit", "-m", msg]).unwrap();
        run_in(&repo.path, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string()
    }

    fn verdicts(
        repo: &Repo,
        prs: &dyn PrLookup,
        states: &HashMap<String, String>,
    ) -> HashMap<String, String> {
        let opts = CleanOptions {
            branches: true,
            ..Default::default()
        };
        let claimed = HashSet::new();
        let input = ScanInput {
            repo_root: Some(&repo.path),
            scan_root: &repo.path,
            opts: &opts,
            prs,
            pr_lookup_available: true,
            claimed_items: &claimed,
            item_states: states,
            live: &[],
        };
        classify_local(&input, &repo.path, "master")
            .into_iter()
            .map(|b| {
                let v = match b.verdict {
                    Verdict::Merged(r) => format!("merged: {r}"),
                    Verdict::Skip(r) => format!("skip: {r}"),
                };
                (b.name, v)
            })
            .collect()
    }

    #[test]
    fn ancestor_branch_is_merged_and_default_is_protected() {
        let repo = init_repo_with_branch("master");
        run_in(&repo.path, &["branch", "done"]).unwrap();
        let v = verdicts(&repo, &NoPrLookup, &HashMap::new());
        assert!(v["done"].starts_with("merged:"), "{v:?}");
        assert!(v["master"].starts_with("skip:"), "{v:?}");
    }

    #[test]
    fn patch_equivalent_branch_is_merged() {
        let repo = init_repo_with_branch("master");
        run_in(&repo.path, &["switch", "-c", "feat"]).unwrap();
        commit(&repo, "a.txt", "a\n", "add a");
        run_in(&repo.path, &["switch", "master"]).unwrap();
        commit(&repo, "other.txt", "o\n", "unrelated");
        run_in(&repo.path, &["cherry-pick", "feat"]).unwrap();
        let v = verdicts(&repo, &NoPrLookup, &HashMap::new());
        assert!(v["feat"].starts_with("merged:"), "{v:?}");
    }

    #[test]
    fn squash_merged_branch_needs_a_merged_pr_containing_the_local_tip() {
        let repo = init_repo_with_branch("master");
        run_in(&repo.path, &["switch", "-c", "feat"]).unwrap();
        commit(&repo, "a.txt", "a\n", "one");
        let tip = commit(&repo, "b.txt", "b\n", "two");
        run_in(&repo.path, &["switch", "master"]).unwrap();
        run_in(&repo.path, &["merge", "--squash", "feat"]).unwrap();
        run_in(&repo.path, &["commit", "-m", "squashed (#9)"]).unwrap();

        let none = verdicts(&repo, &NoPrLookup, &HashMap::new());
        assert!(
            none["feat"].starts_with("skip:"),
            "no PR info => not provably merged: {none:?}"
        );
        let merged = pr("feat", 9, PrState::Merged, &tip);
        assert_eq!(
            verdicts(&repo, &merged, &HashMap::new())["feat"],
            "merged: PR #9 merged"
        );
    }

    #[test]
    fn local_commits_beyond_the_merged_pr_head_block_deletion() {
        let repo = init_repo_with_branch("master");
        run_in(&repo.path, &["switch", "-c", "feat"]).unwrap();
        let pr_head = commit(&repo, "a.txt", "a\n", "one");
        commit(&repo, "late.txt", "late\n", "pushed after merge");
        run_in(&repo.path, &["switch", "master"]).unwrap();
        commit(&repo, "a.txt", "a squashed\n", "squashed (#9)");
        let prs = pr("feat", 9, PrState::Merged, &pr_head);
        let v = verdicts(&repo, &prs, &HashMap::new());
        assert!(v["feat"].starts_with("skip: PR #9 merged, but"), "{v:?}");
    }

    #[test]
    fn open_and_closed_prs_are_skipped_with_the_pr_number() {
        let repo = init_repo_with_branch("master");
        for b in ["open-one", "closed-one"] {
            run_in(&repo.path, &["switch", "-c", b, "master"]).unwrap();
            commit(&repo, &format!("{b}.txt"), "x\n", b);
        }
        run_in(&repo.path, &["switch", "master"]).unwrap();
        let mut prs = pr("open-one", 1, PrState::Open, "");
        prs.0.extend(pr("closed-one", 2, PrState::Closed, "").0);
        let v = verdicts(&repo, &prs, &HashMap::new());
        assert_eq!(v["open-one"], "skip: open PR #1");
        assert_eq!(v["closed-one"], "skip: PR #2 closed, not merged");
    }

    #[test]
    fn empty_task_branch_with_an_open_item_is_kept() {
        let repo = init_repo_with_branch("master");
        run_in(&repo.path, &["branch", "task/309-quota"]).unwrap();
        run_in(&repo.path, &["branch", "task/12-old"]).unwrap();
        let states = HashMap::from([
            ("309".to_string(), "started".to_string()),
            ("12".to_string(), "completed".to_string()),
        ]);
        let v = verdicts(&repo, &NoPrLookup, &states);
        assert!(
            v["task/309-quota"].starts_with("skip: no commits yet"),
            "{v:?}"
        );
        assert!(v["task/12-old"].starts_with("merged:"), "{v:?}");
    }

    // Review Focus 1: no `origin` remote at all.
    #[test]
    fn works_without_an_origin_remote() {
        let repo = init_repo_with_branch("master");
        assert_eq!(base_ref(&repo.path, "master"), "master");
        run_in(&repo.path, &["branch", "done"]).unwrap();
        let v = verdicts(&repo, &NoPrLookup, &HashMap::new());
        assert!(v["done"].starts_with("merged:"), "{v:?}");
    }

    #[test]
    fn remote_branches_merged_into_default_are_candidates() {
        let remote = tempfile::TempDir::new().unwrap();
        run_in(remote.path(), &["init", "--bare", "-b", "master"]).unwrap();
        let repo = init_repo_with_branch("master");
        let url = remote.path().to_str().unwrap();
        run_in(&repo.path, &["remote", "add", "origin", url]).unwrap();
        run_in(&repo.path, &["branch", "done"]).unwrap();
        run_in(&repo.path, &["switch", "-c", "wip"]).unwrap();
        commit(&repo, "w.txt", "w\n", "wip");
        run_in(&repo.path, &["switch", "master"]).unwrap();
        run_in(&repo.path, &["push", "origin", "master", "done", "wip"]).unwrap();

        let opts = CleanOptions {
            remote: true,
            ..Default::default()
        };
        let (claimed, states) = (HashSet::new(), HashMap::new());
        let input = ScanInput {
            repo_root: Some(&repo.path),
            scan_root: &repo.path,
            opts: &opts,
            prs: &NoPrLookup,
            pr_lookup_available: true,
            claimed_items: &claimed,
            item_states: &states,
            live: &[],
        };
        let (items, skipped) = classify_remote(&input, &repo.path, "master");
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["remote:done"]);
        assert!(items[0].sha.is_some());
        assert!(
            skipped
                .iter()
                .any(|s| s.label == "origin/wip" && s.reason == "not merged"),
            "{skipped:?}"
        );
    }
}
