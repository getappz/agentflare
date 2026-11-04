//! End-to-end: the real `prepare-commit-msg` hook + the built binary stamp an
//! `Agentflare-Review` trailer only when the exact committed tree was attested.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

struct Repo {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    path_env: String,
}

fn git_in(r: &Repo, args: &[&str]) -> std::process::Output {
    let mut c = Command::new("git");
    c.current_dir(&r.dir)
        .args(args)
        .env("PATH", &r.path_env)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("HOME", &r.dir)
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_WORK_TREE");
    c.output().unwrap()
}

fn git(r: &Repo, args: &[&str]) {
    let o = git_in(r, args);
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn write(r: &Repo, f: &str, body: &str) {
    fs::write(r.dir.join(f), body).unwrap();
}

fn repo() -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    symlink(env!("CARGO_BIN_EXE_agentflare"), bin.join("agentflare")).unwrap();
    let path_env = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    fs::create_dir_all(&dir).unwrap();
    let r = Repo {
        _tmp: tmp,
        dir,
        path_env,
    };
    git(&r, &["init", "-q", "-b", "main"]);
    let hook = r.dir.join(".git/hooks/prepare-commit-msg");
    fs::write(&hook, include_str!("../.githooks/prepare-commit-msg")).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    write(&r, "a.txt", "one");
    git(&r, &["add", "a.txt"]);
    git(&r, &["commit", "-qm", "init"]);
    r
}

fn review(r: &Repo, flag: &str) {
    let o = Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .current_dir(&r.dir)
        .args(["git", "review", flag])
        .env("PATH", &r.path_env)
        .env("HOME", &r.dir)
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

fn head_msg(r: &Repo) -> String {
    String::from_utf8(git_in(r, &["log", "-1", "--format=%B"]).stdout).unwrap()
}

fn review_lines(msg: &str) -> Vec<&str> {
    msg.lines()
        .filter(|l| l.starts_with("Agentflare-Review:"))
        .collect()
}

fn stage_change(r: &Repo, body: &str) {
    write(r, "a.txt", body);
    git(r, &["add", "a.txt"]);
}

#[test]
fn each_status_is_stamped_on_the_attested_tree() {
    for (flag, want) in [
        (
            "--agent-reviewed",
            "Agentflare-Review: agent-reviewed (iter:1)",
        ),
        ("--vouch", "Agentflare-Review: vouched (iter:1)"),
        ("--skip", "Agentflare-Review: skipped (iter:1)"),
    ] {
        let r = repo();
        stage_change(&r, "two");
        review(&r, flag);
        git(&r, &["commit", "-qm", "change"]);
        assert_eq!(review_lines(&head_msg(&r)), vec![want], "{flag}");
    }
}

#[test]
fn restaging_after_attesting_yields_no_trailer() {
    let r = repo();
    stage_change(&r, "two");
    review(&r, "--vouch");
    stage_change(&r, "three");
    git(&r, &["commit", "-qm", "change"]);
    assert!(review_lines(&head_msg(&r)).is_empty());
}

/// `git commit <path>` runs the hook with a temporary index (HEAD + that
/// path), which differs from the real index when something else is staged.
/// The attestation is for the temporary index's tree, so the trailer only
/// appears if `tree_hash` honours `GIT_INDEX_FILE`.
#[test]
fn partial_commit_hashes_the_temporary_index_not_the_real_one() {
    let r = repo();
    stage_change(&r, "two");
    review(&r, "--vouch");
    // Real index now differs from the attested tree; the path commit's
    // temporary index (HEAD + a.txt) still equals it.
    write(&r, "b.txt", "b");
    git(&r, &["add", "b.txt"]);
    git(&r, &["commit", "-qm", "only a", "--", "a.txt"]);
    assert_eq!(review_lines(&head_msg(&r)).len(), 1);
}

fn out(r: &Repo, args: &[&str]) -> String {
    String::from_utf8(git_in(r, args).stdout)
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn cherry_pick_replay_gets_no_review_trailer() {
    let r = repo();
    git(&r, &["checkout", "-qb", "side"]);
    stage_change(&r, "two");
    // No hook: the replayed message must carry no trailers of its own, or
    // append_trailers' already-stamped guard would mask the sequencer guard.
    git(
        &r,
        &["-c", "core.hooksPath=/dev/null", "commit", "-qm", "x"],
    );
    review(&r, "--vouch"); // clean index == the tree the pick will produce
    git(&r, &["checkout", "-q", "main"]);
    git(&r, &["cherry-pick", "side"]);
    assert_eq!(
        out(&r, &["rev-parse", "HEAD^{tree}"]),
        out(&r, &["rev-parse", "side^{tree}"])
    );
    assert!(review_lines(&head_msg(&r)).is_empty());
}

#[test]
fn rebase_replay_gets_no_review_trailer() {
    let r = repo();
    git(&r, &["checkout", "-qb", "side"]);
    stage_change(&r, "two");
    // No hook: the replayed message must carry no trailers of its own, or
    // append_trailers' already-stamped guard would mask the sequencer guard.
    git(
        &r,
        &["-c", "core.hooksPath=/dev/null", "commit", "-qm", "x"],
    );
    git(&r, &["checkout", "-q", "main"]);
    write(&r, "c.txt", "c");
    git(&r, &["add", "c.txt"]);
    git(&r, &["commit", "-qm", "main moves"]);
    // Attest the exact tree the rebase will produce (main + x), without
    // committing, then throw the scratch state away.
    git(&r, &["checkout", "-qb", "scratch"]);
    git(&r, &["cherry-pick", "-n", "side"]);
    review(&r, "--vouch");
    let expected = out(&r, &["write-tree"]);
    git(&r, &["reset", "-q", "--hard"]);
    git(&r, &["checkout", "-q", "side"]);
    git(&r, &["rebase", "-q", "main"]);
    assert_eq!(out(&r, &["rev-parse", "HEAD^{tree}"]), expected);
    assert!(review_lines(&head_msg(&r)).is_empty());
}

#[test]
fn amend_does_not_duplicate_the_trailer() {
    let r = repo();
    stage_change(&r, "two");
    review(&r, "--vouch");
    git(&r, &["commit", "-qm", "change"]);
    git(&r, &["commit", "-q", "--amend", "--no-edit"]);
    assert_eq!(review_lines(&head_msg(&r)).len(), 1);
}

#[test]
fn merge_commit_gets_no_review_trailer() {
    let r = repo();
    git(&r, &["checkout", "-qb", "side"]);
    write(&r, "b.txt", "b");
    git(&r, &["add", "b.txt"]);
    git(&r, &["commit", "-qm", "side"]);
    git(&r, &["checkout", "-q", "main"]);
    write(&r, "c.txt", "c");
    git(&r, &["add", "c.txt"]);
    git(&r, &["commit", "-qm", "main"]);
    git(&r, &["merge", "--no-commit", "--no-ff", "side"]);
    review(&r, "--vouch");
    git(&r, &["commit", "--no-edit", "-q"]);
    assert!(review_lines(&head_msg(&r)).is_empty());
}

#[test]
fn review_without_a_status_writes_no_attestation() {
    let r = repo();
    stage_change(&r, "two");
    let _ = Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .current_dir(&r.dir)
        .args(["git", "review"])
        .env("HOME", &r.dir)
        .output()
        .unwrap();
    assert!(!Path::new(&r.dir.join(".git/agentflare/attestations")).exists());
    git(&r, &["commit", "-qm", "change"]);
    assert!(review_lines(&head_msg(&r)).is_empty());
}
