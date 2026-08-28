use flare_insights::{config::InsightsConfig, ingest::IngestManager, store::InsightsStore};

#[test]
fn ingest_real_claude_and_opencode_if_present() {
    let config = InsightsConfig::default();
    let has_claude = config
        .sources
        .get("claude_code")
        .map(|p| p.exists())
        .unwrap_or(false);
    let has_opencode = config
        .sources
        .get("opencode")
        .map(|p| p.exists())
        .unwrap_or(false);
    if !has_claude && !has_opencode {
        eprintln!("skipping real_data test - no claude/opencode data");
        return;
    }

    let mgr = IngestManager::new();
    let bundle = mgr.scan_all_flat(&config);

    // DRY: at least one session should exist on this dev machine
    // If not, don't fail - just check that scan didn't error
    if bundle.sessions.is_empty() {
        eprintln!("no sessions found, but scan succeeded");
        return;
    }

    for s in &bundle.sessions {
        assert!(!s.id.is_empty());
        assert!(!s.project.is_empty());
    }

    // Verify store round-trip doesn't panic
    let store = InsightsStore::open_in_memory().unwrap();
    for s in &bundle.sessions {
        let _ = store.upsert_session(s);
    }
    let _ = store.upsert_turns_batch(&bundle.turns);
    let _ = store.upsert_tool_calls_batch(&bundle.tool_calls);
    let _ = store.upsert_file_events_batch(&bundle.file_events);

    let listed = store.list_sessions(5, 0).unwrap();
    assert!(!listed.is_empty());

    // search should not error
    let _ = flare_insights::search::search(
        &store,
        &flare_insights::search::SearchOptions {
            query: "test".into(),
            limit: 5,
            ..Default::default()
        },
    );

    let tools = store.list_tool_calls(1000).unwrap();
    let files = store.list_file_events(1000).unwrap();
    let _ =
        flare_insights::analytics::compute_analytics_with_tools(&bundle.sessions, &tools, &files);
}
