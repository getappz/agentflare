/// Item #164's incident, reproduced at the unit level: an auth-expiry
/// failure must NOT go through `classify_and_cooldown`'s retry path --
/// `handle_auth_expired` must short-circuit it to `fatal: true` instead,
/// so the job queue's retry budget is never consumed against a
/// credential that will fail identically every time.
#[test]
fn handle_auth_expired_ignores_non_auth_expiry_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_root).unwrap();
    init_test_repo(&repo_root);
    let mcp = AgentflareMcp::for_test(
        tmp.path().join("backend.db"),
        repo_root,
        tmp.path().join("project.json"),
    );
    let item = mcp.with_backend_db(|conn| seeded_item(&mcp, conn)).unwrap();
    let mut log = Vec::new();
    assert!(handle_auth_expired(&mcp, &item, "something went wrong", &mut log).is_none());
}

#[test]
fn handle_auth_expired_fails_fatally_and_labels_the_item() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_root).unwrap();
    init_test_repo(&repo_root);
    let mcp = AgentflareMcp::for_test(
        tmp.path().join("backend.db"),
        repo_root,
        tmp.path().join("project.json"),
    );
    let item = mcp.with_backend_db(|conn| seeded_item(&mcp, conn)).unwrap();

    let mut log = Vec::new();
    let outcome = handle_auth_expired(
        &mcp,
        &item,
        "Error: session expired, please re-authenticate",
        &mut log,
    )
    .expect("auth-expiry-shaped message must be handled");
    assert!(outcome.fatal);
    assert_eq!(outcome.retry_after_secs, None);
    assert!(String::from_utf8(log).unwrap().contains("expired-auth failure"));

    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item.id))
        .unwrap()
        .unwrap();
    let label_names: Vec<String> = mcp
        .with_backend_db(|conn| {
            labels
                .iter()
                .filter_map(|id| agentflare_backend::label::get(conn, id).ok())
                .map(|l| l.name)
                .collect()
        })
        .unwrap();
    assert!(label_names.contains(&AUTH_EXPIRED_LABEL.to_string()));
}

#[test]
fn ensure_auth_expired_label_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_root).unwrap();
    init_test_repo(&repo_root);
    let mcp = AgentflareMcp::for_test(
        tmp.path().join("backend.db"),
        repo_root,
        tmp.path().join("project.json"),
    );
    let first = ensure_auth_expired_label(&mcp).expect("label must be created");
    let second = ensure_auth_expired_label(&mcp).expect("label must be found, not duplicated");
    assert_eq!(first, second);
}
