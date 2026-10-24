//! Item #373: `done` when the item's PR was merged while its job was still
//! running.
//!
//! Split into its own file to keep `action_tests.rs` under the repo's LOC
//! gate (`scripts/loc-gate.sh`), mirroring the other `item_*_tests` files.

use super::*;

#[test]
fn item_done_completes_an_item_whose_pr_merged_while_the_job_was_running() {
    // Item #373: a human merged the item's PR while the job was still
    // running, and the head branch was deleted with it. That is a *completed*
    // merge, not a failed push -- `done` used to try to push a branch that no
    // longer existed, and the item was left wedged in "started" with its PR
    // record stripped and nothing able to find the PR again (item #283 /
    // PR #910, and item #173 / PR #666).
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let run_git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo_root)
            .output()
            .unwrap()
    };
    run_git(&["init", "-b", "master"]);
    run_git(&["config", "user.email", "test@test.com"]);
    run_git(&["config", "user.name", "Test"]);
    std::fs::write(repo_root.join("a.txt"), "a").unwrap();
    run_git(&["add", "-A"]);
    run_git(&["commit", "-m", "a"]);
    // A GitHub-shaped remote: the merge lookup has to resolve this repo to a
    // repo, or it never happens. Nothing in this test pushes to it.
    run_git(&["remote", "add", "origin", "https://github.com/o/r.git"]);
    let head = String::from_utf8_lossy(&run_git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(
            200,
            &format!(
                r#"{{"number":910,"html_url":"u","state":"closed","title":"t","merged_at":"2026-10-17T00:00:00Z","head":{{"ref":"task/1-x","sha":"{head}"}}}}"#
            ),
        ),
    ]);
    let client = server.client(Some("tok"));

    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root.clone()),
        github_client_override: Some(client),
        ..Default::default()
    };

    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Fix CI"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let seq = created["sequence_id"].as_i64().unwrap();
    let branch = format!("task/{seq}-x");
    s.with_backend_db(|conn| {
        crate::mcp_server::merge_item_metadata(conn, &item_id, |map| {
            map.insert(
                "pr".into(),
                serde_json::json!({"number": 910, "branch": branch}),
            );
        })
        .unwrap();
    })
    .unwrap();

    // The worktree and branch a claim would have created.
    let worktree = repo_root
        .join(".worktrees")
        .join("task")
        .join(seq.to_string());
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    run_git(&[
        "worktree",
        "add",
        "-b",
        &branch,
        worktree.to_str().unwrap(),
        "master",
    ]);

    let owner = crate::claims::owner_id();
    s.with_backend_db(|conn| {
        agentflare_backend::item::claim(
            conn,
            &item_id,
            &owner,
            crate::claims::now(),
            crate::claims::ttl_secs(),
        )
        .unwrap();
    })
    .unwrap();

    let resp: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        resp["status"], "completed",
        "the merged PR completes the item"
    );
    assert_eq!(resp["promoted"], true);
    assert_eq!(resp["pr_number"], 910);

    let fetched: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        !fetched["completed_at"].is_null(),
        "the item must not be left in progress: {fetched}"
    );
    let metadata: serde_json::Value =
        serde_json::from_str(fetched["metadata"].as_str().unwrap()).unwrap();
    assert_eq!(
        metadata["pr"]["number"], 910,
        "the PR record has to survive: it is what lets the sweep find this PR again"
    );
    assert!(
        !worktree.exists(),
        "the worktree is cleaned up once the item is done"
    );
    let reqs = server.requests();
    assert_eq!(
        reqs.len(),
        1,
        "the merge is looked up once and nothing else is asked of GitHub"
    );
    assert_eq!(reqs[0].path, "/repos/o/r/pulls/910");
}

/// A claimed item with a worktree holding one real commit, in a repo with a
/// GitHub-shaped remote. `pr` is recorded as `metadata.pr.number` when set.
/// Returns (mcp, item id, worktree path, mock server, temp dirs to keep alive).
fn claimed_item_with_commit(
    pr: Option<u64>,
    responses: Vec<crate::github::test_support::MockResponse>,
) -> (
    AgentflareMcp,
    String,
    std::path::PathBuf,
    crate::github::test_support::MockServer,
    (tempfile::TempDir, tempfile::TempDir),
) {
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
    };
    git(&repo_root, &["init", "-b", "master"]);
    git(&repo_root, &["config", "user.email", "test@test.com"]);
    git(&repo_root, &["config", "user.name", "Test"]);
    std::fs::write(repo_root.join("a.txt"), "a").unwrap();
    git(&repo_root, &["add", "-A"]);
    git(&repo_root, &["commit", "-m", "a"]);
    git(
        &repo_root,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    let server = crate::github::test_support::MockServer::start(responses);
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root.clone()),
        github_client_override: Some(server.client(Some("tok"))),
        ..Default::default()
    };
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Fix CI"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let seq = created["sequence_id"].as_i64().unwrap();
    let branch = format!("task/{seq}-x");
    if let Some(number) = pr {
        s.with_backend_db(|conn| {
            crate::mcp_server::merge_item_metadata(conn, &item_id, |map| {
                map.insert(
                    "pr".into(),
                    serde_json::json!({"number": number, "branch": branch}),
                );
            })
            .unwrap();
        })
        .unwrap();
    }
    let worktree = repo_root
        .join(".worktrees")
        .join("task")
        .join(seq.to_string());
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git(
        &repo_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            worktree.to_str().unwrap(),
            "master",
        ],
    );
    std::fs::write(worktree.join("b.txt"), "b").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-m", "work"]);
    let owner = crate::claims::owner_id();
    s.with_backend_db(|conn| {
        agentflare_backend::item::claim(
            conn,
            &item_id,
            &owner,
            crate::claims::now(),
            crate::claims::ttl_secs(),
        )
        .unwrap();
    })
    .unwrap();
    (s, item_id, worktree, server, (tmp, repo_dir))
}

fn done_without_push(s: &AgentflareMcp, item_id: &str) -> serde_json::Value {
    serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.to_string()),
            push: Some(false),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn item_done_without_push_keeps_an_item_with_an_open_pr_in_review() {
    // Item #396: `push=false` used to complete the item whatever its PR was
    // doing, hiding an open PR from the review sweep for good.
    let open_pr = r#"{"number":77,"html_url":"u","state":"open","title":"t","mergeable":true,"mergeable_state":"behind","head":{"ref":"b","sha":"abc"}}"#;
    // First lookup: the #373 merged check; second: the open-PR check.
    let (s, item_id, worktree, server, _keep) = claimed_item_with_commit(
        Some(77),
        vec![
            crate::github::test_support::MockResponse::json(200, open_pr),
            crate::github::test_support::MockResponse::json(200, open_pr),
        ],
    );

    let resp = done_without_push(&s, &item_id);

    assert_eq!(resp["status"], "in_review", "{resp}");
    assert!(
        worktree.exists(),
        "the worktree stays for follow-up commits"
    );
    let owner = crate::claims::owner_id();
    s.with_backend_db(|conn| {
        assert!(
            agentflare_backend::claim::is_owner(conn, &item_id, &owner).unwrap(),
            "the claim stays held"
        );
        // The sweep's own query: the item must be in its batch.
        let project = s.resolve_project(conn).unwrap();
        let batch = crate::supervisor::build_review_batch(
            conn,
            agentflare_backend::project_dir::ProjectDir {
                project_id: project.id,
                folder_path: String::new(),
                updated_at: 0,
            },
        )
        .unwrap()
        .expect("project has an in_review state");
        assert!(
            batch.items.iter().any(|i| i.id == item_id),
            "the sweep sees the item and so its PR"
        );
    })
    .unwrap();
    assert!(
        server
            .requests()
            .iter()
            .all(|r| r.path.ends_with("/pulls/77"))
    );
}

#[test]
fn item_done_without_push_completes_an_item_with_no_pr() {
    let (s, item_id, worktree, server, _keep) = claimed_item_with_commit(None, vec![]);

    let resp = done_without_push(&s, &item_id);

    assert_eq!(resp["status"], "completed", "{resp}");
    assert!(!worktree.exists(), "cleaned up as before");
    // (the pre-existing #373 branch lookup may still ask GitHub; the open-PR check must not)
    assert!(
        server
            .requests()
            .iter()
            .all(|r| !r.path.contains("/pulls/"))
    );
}
