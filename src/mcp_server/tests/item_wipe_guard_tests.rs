//! Item #689: a claimed worktree that lost every tracked file must never be
//! committed by `done`'s auto-commit, pushed, or cleared for its own
//! claimant.

use super::*;

/// Through `run_in`, which scrubs `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE`:
/// run from a hook, a raw `git` would act on the real checkout, and the
/// `restore` below would destroy its uncommitted edits.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
    flare_git_core::shell::run_in(dir, args).unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
}

/// Commits `n` tracked files on the item's branch -- the "real work" a wipe
/// then deletes.
fn commit_tracked_files(worktree: &std::path::Path, n: usize) {
    for i in 0..n {
        std::fs::write(worktree.join(format!("f{i}.txt")), "tracked\n").unwrap();
    }
    git(worktree, &["add", "-A"]);
    git(worktree, &["commit", "-m", "real work"]);
}

fn delete_tracked_files(worktree: &std::path::Path, n: usize) {
    for i in 0..n {
        std::fs::remove_file(worktree.join(format!("f{i}.txt"))).unwrap();
    }
}

fn done(s: &AgentflareMcp, item_id: &str, force: bool) -> Result<String, ErrorData> {
    s.item(Parameters(ItemRequest {
        action: "done".into(),
        id: Some(item_id.to_string()),
        summary: Some("finished".into()),
        force: force.then_some(true),
        force_reason: force.then(|| "the deletion is the task".to_string()),
        ..Default::default()
    }))
}

fn comment_bodies(s: &AgentflareMcp, item_id: &str) -> String {
    s.comment(Parameters(CommentRequest {
        action: "list".into(),
        item_id: Some(item_id.to_string()),
        ..Default::default()
    }))
    .unwrap()
}

#[test]
fn item_done_refuses_to_push_a_tip_commit_that_mass_deletes_tracked_files() {
    let (s, _tmp, repo_dir, item_id, _project_id, worktree) =
        mcp_with_claimed_item("Wipe push guard");
    commit_tracked_files(&worktree, 30);
    // What the SDD checkpoint did to item #686: the wipe, committed.
    delete_tracked_files(&worktree, 30);
    // Net-zero against the target would read as "nothing committed".
    std::fs::write(
        worktree.join("kept.txt"),
        "x
",
    )
    .unwrap();
    git(&worktree, &["add", "-A"]);
    git(
        &worktree,
        &["commit", "-m", "wip(sdd-loop): task 0 checkpoint"],
    );
    let branch = git(&worktree, &["branch", "--show-current"]);

    let err = done(&s, &item_id, false).unwrap_err();
    assert!(err.message.contains("mass deletion"), "{}", err.message);
    assert!(
        git(repo_dir.path(), &["ls-remote", "origin", &branch]).is_empty(),
        "the wipe must not reach the remote"
    );
    assert!(
        comment_bodies(&s, &item_id).contains("push refused: mass deletion"),
        "the refusal must be posted on the item"
    );
    assert!(worktree.exists(), "the worktree is left for inspection");

    // An intended mass deletion still has a way through, with a reason.
    let forced = done(&s, &item_id, true).expect("force with a reason must publish");
    assert!(forced.contains("\"completed\""), "{forced}");
    assert!(!git(repo_dir.path(), &["ls-remote", "origin", &branch]).is_empty());
    // R3: the caller owns the claim, so `force_takeover` posts nothing; the
    // override itself must still leave an audit record.
    let comments = comment_bodies(&s, &item_id);
    assert!(comments.contains("OVERRIDDEN"), "{comments}");
    assert!(comments.contains("the deletion is the task"), "{comments}");
    assert!(comments.contains(&branch), "{comments}");
}

// R5: the `allow-mass-deletion` label waives both guards without `force`.
#[test]
fn item_done_honours_the_allow_mass_deletion_label() {
    let (s, _tmp, repo_dir, item_id, project_id, worktree) = mcp_with_claimed_item("Wipe label");
    commit_tracked_files(&worktree, 30);
    delete_tracked_files(&worktree, 30);
    std::fs::write(worktree.join("kept.txt"), "x").unwrap();
    git(&worktree, &["add", "-A"]);
    git(&worktree, &["commit", "-m", "deliberate deletion"]);
    let branch = git(&worktree, &["branch", "--show-current"]);
    assert!(done(&s, &item_id, false).is_err(), "unlabelled is refused");

    s.with_backend_db(|conn| {
        let project = agentflare_backend::project::get(conn, &project_id).unwrap();
        agentflare_backend::label::create(
            conn,
            agentflare_backend::label::CreateLabel {
                project_id: Some(project_id.clone()),
                workspace_id: project.workspace_id,
                name: flare_git_core::worktree::ALLOW_MASS_DELETION_LABEL.into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        agentflare_backend::item::add_label_by_name(
            conn,
            &item_id,
            flare_git_core::worktree::ALLOW_MASS_DELETION_LABEL,
        )
        .unwrap();
    })
    .unwrap();
    assert!(s.item_allows_mass_deletion(&item_id));
    done(&s, &item_id, false).expect("the label waives the push guard");
    assert!(!git(repo_dir.path(), &["ls-remote", "origin", &branch]).is_empty());
    assert!(comment_bodies(&s, &item_id).contains("OVERRIDDEN"));
}

#[test]
fn item_done_refuses_to_auto_commit_a_wiped_worktree() {
    let (s, _tmp, _repo_dir, item_id, _project_id, worktree) =
        mcp_with_claimed_item("Wipe auto-commit guard");
    commit_tracked_files(&worktree, 30);
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    delete_tracked_files(&worktree, 30);

    let err = done(&s, &item_id, false).unwrap_err();
    assert!(err.message.contains("mass deletion"), "{}", err.message);
    assert_eq!(
        git(&worktree, &["rev-parse", "HEAD"]),
        head,
        "the wipe must not be committed"
    );
    git(
        &worktree,
        &["restore", "--worktree", "--source=HEAD", "--", "."],
    );
    assert!(
        worktree.join("f0.txt").exists(),
        "still recoverable from HEAD"
    );
}

#[test]
fn item_claim_by_the_live_claimant_never_clears_its_worktree() {
    let (s, _tmp, _repo_dir, item_id, _project_id, worktree) =
        mcp_with_claimed_item("Live claimant guard");
    std::fs::write(worktree.join("precious.txt"), "uncommitted work").unwrap();
    // Structurally broken beyond `worktree repair` (pointer and admin entry
    // both gone), which a fresh claim would snapshot and clear.
    std::fs::remove_file(worktree.join(".git")).unwrap();
    std::fs::remove_dir_all(_repo_dir.path().join(".git/worktrees")).unwrap();

    let reclaimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let error = reclaimed["worktree_error"].as_str().unwrap_or_default();
    assert!(error.contains("live claim"), "{reclaimed}");
    assert!(
        worktree.join("precious.txt").exists(),
        "the claimant's own worktree was cleared"
    );

    // R2: the refusal protects the claim -- it must not release it. A second
    // claim by the same owner behaves the same and the lease survives both.
    let owner = crate::claims::owner_id();
    let again = s
        .item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap();
    assert!(again.contains("live claim"), "{again}");
    assert!(worktree.join("precious.txt").exists());
    let still_held = s
        .with_backend_db(|conn| crate::mcp_server::item_force::holds_claim(conn, &item_id, &owner))
        .unwrap();
    assert!(
        still_held,
        "the refusal released the claim it was protecting"
    );
}
