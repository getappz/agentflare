use super::*;

/// Claimed item in a throwaway repo; `verdict` = `(verdict, evidence)` of the
/// single criterion `a`, or `None` for no acceptance_criteria at all.
/// `require` sets `metadata.require_verify`.
fn setup(
    require: bool,
    verdict: Option<(&str, &str)>,
) -> (AgentflareMcp, String, tempfile::TempDir, tempfile::TempDir) {
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
    run_git(&["commit", "--allow-empty", "-m", "initial"]);
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root),
        ..Default::default()
    };
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    s.with_backend_db(|conn| {
        let item = agentflare_backend::item::get(conn, &item_id).unwrap();
        let mut meta = serde_json::json!({ "require_verify": require });
        if let Some((v, e)) = verdict {
            let ac = serde_json::json!([{"id": "a", "text": "does the thing"}]);
            meta["acceptance_criteria"] = ac.clone();
            meta["verify"] = serde_json::json!({
                "content_hash": crate::review::verify::verify_content_hash(&item.description, &ac),
                "criteria": [{"id": "a", "verdict": v, "evidence": e}],
            });
        }
        agentflare_backend::item::update(
            conn,
            &item_id,
            agentflare_backend::item::UpdateItem {
                metadata: Some(meta.to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    })
    .unwrap();
    s.item(Parameters(ItemRequest {
        action: "claim".into(),
        id: Some(item_id.clone()),
        ..Default::default()
    }))
    .unwrap();
    (s, item_id, tmp, repo_dir)
}

fn call(s: &AgentflareMcp, action: &str, id: &str, force: bool) -> serde_json::Value {
    let r = s
        .item(Parameters(ItemRequest {
            action: action.into(),
            id: Some(id.to_string()),
            force: force.then_some(true),
            force_reason: force.then(|| "test override".to_string()),
            ..Default::default()
        }))
        .unwrap();
    serde_json::from_str(&r).unwrap()
}

fn group_of(s: &AgentflareMcp, id: &str) -> String {
    s.with_backend_db(|conn| {
        let i = agentflare_backend::item::get(conn, id).unwrap();
        agentflare_backend::state::get(conn, &i.state_id)
            .unwrap()
            .group_name
    })
    .unwrap()
}

fn comments(s: &AgentflareMcp, id: &str) -> Vec<String> {
    s.with_backend_db(|conn| {
        agentflare_backend::comment::list_by_item(conn, id)
            .unwrap()
            .into_iter()
            .map(|c| c.body)
            .collect()
    })
    .unwrap()
}

#[test]
fn done_with_no_criteria_lands_in_needs_input_even_twice() {
    let (s, id, _t, _r) = setup(true, None);
    for _ in 0..2 {
        let r = call(&s, "done", &id, false);
        assert_eq!(r["done"], false);
        assert_eq!(r["status"], "needs_input");
    }
    assert_eq!(group_of(&s, &id), "needs_input");
    // The repeat call must not spam the item's history.
    let n = comments(&s, &id)
        .iter()
        .filter(|b| b.contains("verification blocked"))
        .count();
    assert_eq!(n, 1);
}

#[test]
fn done_with_failing_criterion_comments_verdict_evidence_and_gates_human() {
    let (s, id, _t, _r) = setup(true, Some(("fail", "src/a.rs:7 returns early")));
    let r = call(&s, "done", &id, false);
    assert_eq!(r["status"], "needs_input");
    assert_eq!(group_of(&s, &id), "needs_input");
    let body = comments(&s, &id)
        .into_iter()
        .find(|b| b.contains("verification blocked"))
        .expect("verdict comment");
    assert!(body.contains("- a: fail"), "{body}");
    assert!(body.contains("src/a.rs:7 returns early"), "{body}");
    let gated = s
        .with_backend_db(|conn| {
            let gate = agentflare_backend::label::list_by_project(
                conn,
                &agentflare_backend::item::get(conn, &id).unwrap().project_id,
            )
            .unwrap()
            .into_iter()
            .find(|l| l.name == crate::supervisor::NEEDS_HUMAN_GATE_LABEL)
            .map(|l| l.id);
            gate.is_some_and(|g| {
                agentflare_backend::item::list_labels(conn, &id)
                    .unwrap()
                    .contains(&g)
            })
        })
        .unwrap();
    assert!(gated, "needs-human-gate label must be on the item");
}

#[test]
fn done_with_all_pass_and_evidence_is_unchanged() {
    let (s, id, _t, _r) = setup(true, Some(("pass", "src/a.rs:7")));
    let r = call(&s, "done", &id, false);
    // No commits in the throwaway repo: the pre-existing outcome.
    assert_eq!(r["status"], "unchanged");
    assert_ne!(group_of(&s, &id), "needs_input");
}

#[test]
fn enforcement_off_ignores_missing_criteria() {
    let (s, id, _t, _r) = setup(false, None);
    let r = call(&s, "done", &id, false);
    assert_eq!(r["status"], "unchanged");
    assert_ne!(group_of(&s, &id), "needs_input");
}

#[test]
fn speculative_check_merge_leaves_an_unmerged_in_review_item_alone() {
    let (s, id, _t, _r) = setup(true, Some(("uncertain", "")));
    s.with_backend_db(|conn| {
        agentflare_backend::item::mark_in_review(conn, &id, &crate::claims::owner_id()).unwrap();
    })
    .unwrap();
    let r = call(&s, "check_merge", &id, false);
    assert_eq!(r["promoted"], false);
    assert_eq!(r["reason"], "PR not merged yet");
    assert_eq!(group_of(&s, &id), "in_review");
}

#[test]
fn forced_check_merge_cannot_bypass_the_gate() {
    let (s, id, _t, _r) = setup(true, Some(("fail", "src/a.rs:1")));
    s.with_backend_db(|conn| {
        let item = agentflare_backend::item::get(conn, &id).unwrap();
        let backlog =
            agentflare_backend::state::first_in_group(conn, &item.project_id, "backlog").unwrap();
        agentflare_backend::item::update_state(conn, &id, &backlog.id).unwrap();
    })
    .unwrap();
    let r = call(&s, "check_merge", &id, true);
    assert_eq!(r["promoted"], false);
    assert_eq!(r["status"], "needs_input");
    assert_eq!(group_of(&s, &id), "needs_input");
}
