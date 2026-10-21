use super::*;

#[test]
fn item_done_with_require_verify_and_no_criteria_lands_in_needs_input() {
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
        agentflare_backend::item::update(
            conn,
            &item_id,
            agentflare_backend::item::UpdateItem {
                metadata: Some(r#"{"require_verify":true}"#.to_string()),
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
    for _ in 0..2 {
        let result: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "done".into(),
                id: Some(item_id.clone()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["done"], false);
        assert_eq!(result["status"], "needs_input");
    }
    let group = s
        .with_backend_db(|conn| {
            let i = agentflare_backend::item::get(conn, &item_id).unwrap();
            agentflare_backend::state::get(conn, &i.state_id)
                .unwrap()
                .group_name
        })
        .unwrap();
    assert_eq!(group, "needs_input");
}
