use flare_insights::{
    config::InsightsConfig,
    ingest::{watcher::InsightsWatcher, Adapter, FileCursors},
    store::InsightsStore,
};
use rusqlite::{params, Connection};

#[test]
fn project_syncs_do_not_consume_each_others_or_global_cursors() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("opencode.db");
    let project_a = tmp.path().join("project");
    let project_b = tmp.path().join("project-other");
    let mut conn = Connection::open(&source).unwrap();
    conn.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_updated INTEGER);
        CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
        CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, data TEXT);").unwrap();
    let tx = conn.transaction().unwrap();
    for (id, directory) in [
        ("a", project_a.clone()),
        ("nested", project_a.join("src")),
        ("b", project_b.clone()),
    ] {
        tx.execute(
            "INSERT INTO session VALUES (?1, ?2, 1000)",
            params![id, directory.to_string_lossy()],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    drop(conn);
    let store = InsightsStore::open_in_memory().unwrap();
    let mut config = InsightsConfig {
        sources: [("opencode".into(), source)].into(),
        project: Some(project_a),
        ..Default::default()
    };
    assert_eq!(
        InsightsWatcher::rescan_and_store(&config, &store),
        (2, 0, 0)
    );
    assert_eq!(
        InsightsWatcher::rescan_and_store(&config, &store),
        (0, 0, 0)
    );
    config.project = Some(project_b);
    assert_eq!(
        InsightsWatcher::rescan_and_store(&config, &store),
        (1, 0, 0)
    );
    config.project = None;
    assert_eq!(
        InsightsWatcher::rescan_and_store(&config, &store),
        (3, 0, 0)
    );
    assert_eq!(store.list_sessions(10, 0).unwrap().len(), 3);
}

#[test]
fn jsonl_sources_exclude_other_and_unknown_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    for (name, cwd) in [
        ("matching", Some(project.clone())),
        ("other", Some(tmp.path().join("project-other"))),
        ("unknown", None),
    ] {
        let value = serde_json::json!({"type":"user", "role":"user", "cwd":cwd,
            "message":{"role":"user", "content":"hello"},
            "payload":{"role":"user", "content":"hello", "cwd":cwd}});
        std::fs::write(
            tmp.path().join(format!("{name}.jsonl")),
            format!("{value}\n"),
        )
        .unwrap();
    }
    let config = InsightsConfig {
        sources: [
            ("claude_code".into(), tmp.path().to_path_buf()),
            ("codex".into(), tmp.path().to_path_buf()),
        ]
        .into(),
        project: Some(project),
        ..Default::default()
    };
    for adapter in [
        &flare_insights::ingest::claude::ClaudeAdapter as &dyn Adapter,
        &flare_insights::ingest::codex::CodexAdapter,
    ] {
        let bundle = adapter
            .scan(&config, &FileCursors::default(), &mut |_, _| {})
            .unwrap();
        assert_eq!(bundle.sessions.len(), 1, "{}", adapter.source_name());
        assert_eq!(bundle.sessions[0].id, "matching");
    }
}

#[test]
fn scoped_opencode_finds_an_older_session_beyond_the_global_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("opencode.db");
    let project = tmp.path().join("project");
    let mut conn = Connection::open(&source).unwrap();
    conn.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, time_updated INTEGER, parent_id TEXT);
        CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, data TEXT);
        CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, data TEXT);").unwrap();
    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT INTO session VALUES ('older', ?1, 1, NULL)",
        [project.to_string_lossy()],
    )
    .unwrap();
    for i in 0..1001 {
        tx.execute(
            "INSERT INTO session VALUES (?1, '/unrelated', 2000, 'unrelated-parent')",
            [format!("noise-{i}")],
        )
        .unwrap();
    }
    tx.execute(
        "INSERT INTO session VALUES ('child', ?1, 1, 'older')",
        [project.to_string_lossy()],
    )
    .unwrap();
    tx.commit().unwrap();
    drop(conn);
    let config = InsightsConfig {
        sources: [("opencode".into(), source)].into(),
        project: Some(project),
        ..Default::default()
    };
    let bundle = flare_insights::ingest::opencode::OpenCodeAdapter
        .scan(&config, &FileCursors::default(), &mut |_, _| {})
        .unwrap();
    assert_eq!(bundle.sessions.len(), 2);
    assert_eq!(bundle.subagents.len(), 1);
    assert_eq!(bundle.subagents[0].id, "child");
    assert_eq!(
        bundle
            .sessions
            .iter()
            .find(|s| s.id == "older")
            .unwrap()
            .subagent_count,
        1
    );
}

#[test]
fn antigravity_scope_uses_native_workspace_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let brain = tmp.path().join("brain");
    let project = tmp.path().join("project");
    let conn = Connection::open(tmp.path().join("conversation_summaries.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE conversation_summaries (conversation_id TEXT, workspace_uris TEXT);",
    )
    .unwrap();
    for (id, workspace) in [
        ("matching", project.clone()),
        ("other", tmp.path().join("project-other")),
    ] {
        let logs = brain.join(id).join(".system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("transcript.jsonl"), "{\"type\":\"USER_INPUT\",\"content\":\"hello\",\"created_at\":\"2026-01-01T00:00:00Z\"}\n").unwrap();
        let uris = serde_json::to_string(&[url::Url::from_directory_path(workspace)
            .unwrap()
            .to_string()])
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2)",
            params![id, uris],
        )
        .unwrap();
    }
    drop(conn);
    let unrelated = brain.join("other/.system_generated/logs/transcript.jsonl");
    std::fs::write(&unrelated, b"\xff\n").unwrap();
    let mut config = InsightsConfig {
        sources: [("antigravity".into(), brain)].into(),
        project: Some(project),
        ..Default::default()
    };
    let bundle = flare_insights::ingest::antigravity::AntigravityAdapter
        .scan(&config, &FileCursors::default(), &mut |_, _| {})
        .unwrap();
    assert_eq!(bundle.sessions.len(), 1);
    assert_eq!(bundle.sessions[0].id, "matching");
    std::fs::write(
        &unrelated,
        "{\"type\":\"USER_INPUT\",\"content\":\"hello\"}\n",
    )
    .unwrap();
    config.project = None;
    assert_eq!(
        flare_insights::ingest::antigravity::AntigravityAdapter
            .scan(&config, &FileCursors::default(), &mut |_, _| {})
            .unwrap()
            .sessions
            .len(),
        2
    );
}
