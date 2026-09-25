#[allow(unused_imports)]
use super::*;

/// A throwaway repo whose `origin` is `origin_url`, with `master` pushed
/// when the origin is reachable, plus an `AgentflareMcp` pinned to it.
fn repo_with_origin(
    origin_url: Option<&std::path::Path>,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    AgentflareMcp,
) {
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let run_git = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_git(&repo_root, &["init", "-b", "master"]);
    run_git(&repo_root, &["config", "user.email", "test@test.com"]);
    run_git(&repo_root, &["config", "user.name", "Test"]);
    run_git(&repo_root, &["commit", "--allow-empty", "-m", "initial"]);
    if let Some(origin) = origin_url {
        run_git(
            &repo_root,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        if origin.exists() {
            run_git(&repo_root, &["push", "origin", "master"]);
        }
    }
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root.clone()),
        ..Default::default()
    };
    (tmp, repo_dir, repo_root, s)
}

fn claim_and_write_work(s: &AgentflareMcp, name: &str) -> String {
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create(name))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let worktree_path = std::path::PathBuf::from(claimed["worktree_path"].as_str().unwrap());
    std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();
    item_id
}

fn comment_bodies(s: &AgentflareMcp, item_id: &str) -> Vec<String> {
    let comments: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "list".into(),
            item_id: Some(item_id.to_string()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    comments
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["body"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn state_group(s: &AgentflareMcp, item_id: &str) -> String {
    s.with_backend_db(|conn| {
        let item = agentflare_backend::item::get(conn, item_id).unwrap();
        agentflare_backend::state::get(conn, &item.state_id)
            .unwrap()
            .group_name
    })
    .unwrap()
}

// Item #109: a real commit landed on the claimed branch but the push itself
// failed (here: `origin` points at a path that doesn't exist) -- a failure a
// retry may fix. `done` must not fall through to `mark_completed`: it
// hard-errors, comments, and keeps the claim so the retry can land it.
#[test]
fn item_done_reports_a_hard_error_when_the_push_fails() {
    let missing = tempfile::tempdir().unwrap().path().join("gone.git");
    let (_tmp, _repo_dir, _repo_root, s) = repo_with_origin(Some(&missing));
    let item_id = claim_and_write_work(&s, "Test");

    let err = s
        .item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.clone()),
            summary: Some("Did the real work.".into()),
            ..Default::default()
        }))
        .unwrap_err();

    assert!(
        err.message.contains("no PR resulted") || err.message.contains("not marking completed"),
        "error must call out the missing PR: {}",
        err.message
    );
    assert!(
        comment_bodies(&s, &item_id)
            .iter()
            .any(|b| b.contains("PR creation failed")),
        "a PR-failure comment must be posted on the item"
    );
    let group = state_group(&s, &item_id);
    assert_ne!(
        group, "completed",
        "a failed push must not complete the item"
    );
    assert_ne!(group, "in_review", "no PR means not in_review");
}

// A repo whose `origin` isn't GitHub (here: a local bare repo) can never get
// a PR, no matter how often `done` is retried -- erroring forever wedged
// every such item. Once the branch is pushed, `done` completes it, records
// why there is no PR, and releases the lease.
#[test]
fn item_done_completes_without_a_pr_when_origin_is_not_github() {
    let origin_dir = tempfile::tempdir().unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "--bare", "-b", "master"])
        .current_dir(origin_dir.path())
        .output()
        .unwrap();
    assert!(init.status.success());
    let (_tmp, _repo_dir, _repo_root, s) = repo_with_origin(Some(origin_dir.path()));
    let item_id = claim_and_write_work(&s, "Test");

    let resp: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.clone()),
            summary: Some("Did the real work.".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(resp["status"], "completed", "{resp}");
    assert!(resp["no_pr"].is_string(), "{resp}");
    assert_eq!(state_group(&s, &item_id), "completed");
    let metadata: serde_json::Value = s
        .with_backend_db(|conn| {
            serde_json::from_str(
                &agentflare_backend::item::get(conn, &item_id)
                    .unwrap()
                    .metadata,
            )
            .unwrap()
        })
        .unwrap();
    assert_eq!(metadata["no_pr"]["pushed"], true, "{metadata}");
    assert!(
        comment_bodies(&s, &item_id)
            .iter()
            .any(|b| b.contains("completed without a PR"))
    );
    let branches = std::process::Command::new("git")
        .args(["branch", "--list"])
        .current_dir(origin_dir.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&branches.stdout).contains("task/"),
        "the work must actually have been pushed before completing"
    );
    s.with_backend_db(|conn| {
        assert!(
            !agentflare_backend::claim::is_owner(conn, &item_id, &crate::claims::owner_id())
                .unwrap(),
            "the lease must be released on completion"
        );
    })
    .unwrap();
}

// No `origin` at all: nothing can be pushed and no PR can result, by
// configuration. The commits stay on the local branch and the item completes
// rather than erroring on every retry.
#[test]
fn item_done_completes_without_a_pr_when_there_is_no_origin() {
    let (_tmp, _repo_dir, _repo_root, s) = repo_with_origin(None);
    let item_id = claim_and_write_work(&s, "Test");

    let resp: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(resp["status"], "completed", "{resp}");
    assert_eq!(state_group(&s, &item_id), "completed");
}

// A finished item has nothing left to publish -- `done` on it must refuse
// with a clear, non-retryable error instead of re-pushing.
#[test]
fn item_done_refuses_an_already_completed_item() {
    let (_tmp, _repo_dir, _repo_root, s) = repo_with_origin(None);
    let item_id = claim_and_write_work(&s, "Test");
    s.item(Parameters(ItemRequest {
        action: "done".into(),
        id: Some(item_id.clone()),
        ..Default::default()
    }))
    .unwrap();

    let err = s
        .item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap_err();
    assert!(err.message.contains("already completed"), "{}", err.message);
}

// Item #512: worktree stayed on bare `task/<N>` while `task_branch_name`
// recomputed a slugged ref after a rename — `push_branch` pushed the real
// checkout but `item_done` classified divergence via `task_branch_name`
// alone, so finalize reported success with no push/PR/comment/state change.
#[test]
fn item_done_publishes_when_worktree_branch_differs_from_recomputed_slug() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let origin_dir = tempfile::tempdir().unwrap();
    let run_git = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };
    run_git(origin_dir.path(), &["init", "--bare", "-b", "master"]);
    run_git(&repo_root, &["init", "-b", "master"]);
    run_git(&repo_root, &["config", "user.email", "test@test.com"]);
    run_git(&repo_root, &["config", "user.name", "Test"]);
    run_git(&repo_root, &["commit", "--allow-empty", "-m", "initial"]);
    run_git(
        &repo_root,
        &[
            "remote",
            "add",
            "origin",
            origin_dir.path().to_str().unwrap(),
        ],
    );
    run_git(&repo_root, &["push", "origin", "master"]);

    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root),
        ..Default::default()
    };

    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("!!!"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let worktree_path = std::path::PathBuf::from(claimed["worktree_path"].as_str().unwrap());

    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.clone()),
        name: Some("Renamed Bugfix Item".into()),
        ..Default::default()
    }))
    .unwrap();

    std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();

    let resp: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.clone()),
            summary: Some("Did the real work.".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    // The slug mismatch must not hide the divergence: the real checkout's
    // branch is pushed and the item completes (origin is not GitHub, so
    // without a PR) -- never the silent "unchanged" no-op #512 produced.
    assert_eq!(
        resp["status"], "completed",
        "slug mismatch must not hide divergence: {resp}"
    );
    assert!(resp["no_pr"].is_string(), "{resp}");
    let branches = std::process::Command::new("git")
        .args(["branch", "--list"])
        .current_dir(origin_dir.path())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&branches.stdout).contains("task/"),
        "the real worktree branch must have been pushed"
    );
    assert!(
        comment_bodies(&s, &item_id)
            .iter()
            .any(|b| b.contains("completed without a PR"))
    );
}

// A claim whose worktree can't be created (here: the repo root isn't a git
// repo at all -- a structural, non-retryable failure) must not leave the
// item wedged "started" under a live lease with nowhere to work.
#[test]
fn item_claim_gives_the_claim_back_when_the_worktree_cannot_be_created() {
    let tmp = tempfile::tempdir().unwrap();
    let not_a_repo = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(not_a_repo.path().to_path_buf()),
        ..Default::default()
    };
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let group_before = state_group(&s, &item_id);

    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    assert!(claimed["worktree_error"].is_string(), "{claimed}");
    assert_eq!(claimed["claim_released"], true, "{claimed}");
    assert_eq!(
        state_group(&s, &item_id),
        group_before,
        "prior state restored"
    );
    s.with_backend_db(|conn| {
        assert_eq!(
            agentflare_backend::claim::current_owner(conn, &item_id),
            None
        );
        assert_eq!(
            agentflare_backend::item::get(conn, &item_id)
                .unwrap()
                .assignee_agent,
            None,
            "the claim's assignee change is undone too"
        );
    })
    .unwrap();
}

// An instance pinned to repo B must derive B's project identity from B's
// own remote -- never from the process cwd (this very repository) -- and so
// never write another repo's project id into B's link file.
#[test]
fn a_repo_scoped_instance_resolves_its_project_from_that_repo_not_the_cwd() {
    let (_tmp, _repo_dir, repo_root, _) = repo_with_origin(None);
    let run = |args: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo_root)
                .status()
                .unwrap()
                .success()
        );
    };
    run(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/widget-scoped-test.git",
    ]);
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root.clone()),
        ..Default::default()
    };

    let project = s
        .with_backend_db(|conn| s.resolve_project(conn))
        .unwrap()
        .unwrap();

    assert_eq!(project.name, "widget-scoped-test");
    assert!(
        project
            .external_id
            .as_deref()
            .is_some_and(|k| k.contains("acme/widget-scoped-test")),
        "{:?}",
        project.external_id
    );
}
