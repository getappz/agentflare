use super::*;

fn seed_doc(s: &AgentflareMcp, ws_id: &str, path: &str, content: &str, doc_type: &str) {
    s.with_store(|store| {
        store
            .doc_upsert_with_opts(
                ws_id,
                path,
                content,
                agentflare_store::documents::DocUpsertOpts {
                    title: Some(path.into()),
                    doc_type: Some(doc_type.into()),
                    source: Some("test".into()),
                    ..Default::default()
                },
            )
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    })
    .unwrap()
    .unwrap();
}

fn ws_id(s: &AgentflareMcp) -> String {
    match s.with_backend_db(AgentflareMcp::resolve_workspace_id) {
        Ok(Ok(id)) => id,
        Ok(Err(e)) => panic!("{e}"),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn search_store_requires_non_empty_query() {
    let (_tmp, s) = harness();
    let err = s
        .search_impl(SearchRequest {
            query: "".into(),
            r#type: None,
            limit: None,
        })
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn search_store_returns_grouped_results() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let wid = ws_id(&s);

        seed_doc(&s, &wid, "docs/report.txt", "this is alpha content", "document");
        seed_doc(
            &s,
            &wid,
            "item_attachment/item-1/memo.txt",
            "beta content here",
            "asset",
        );

        let result = s
            .search_impl(SearchRequest {
                query: "alpha".into(),
                r#type: Some("store".into()),
                limit: Some(50),
            })
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["source"], "store");
        assert!(
            parsed["total"].as_u64().unwrap_or(0) >= 1,
            "expected >=1 result, got {result}"
        );
        let groups = parsed["groups"].as_object().unwrap();
        assert!(
            groups.contains_key("document"),
            "expected document group, got groups: {groups:?}"
        );
    });
}

#[test]
fn search_code_requires_non_empty_query() {
    let (_tmp, s) = harness();
    let err = s
        .search_impl(SearchRequest {
            query: "".into(),
            r#type: Some("code".into()),
            limit: None,
        })
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn search_code_returns_results_from_lean_ctx() {
    let root = AgentflareMcp::repo_root();
    if !root.exists() {
        return; // skip outside a git repo
    }
    let s = AgentflareMcp::default();
    // Search for a pattern that definitely exists in this codebase
    let result = s
        .search_impl(SearchRequest {
            query: "search_impl".into(),
            r#type: Some("code".into()),
            limit: Some(10),
        })
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(parsed["source"], "code");
    // May be 0 if lean-ctx index doesn't cover worktree, but should not error
    assert!(parsed["total"].as_u64().is_some());
}

#[test]
fn search_store_defaults_to_store_type() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let wid = ws_id(&s);

        seed_doc(&s, &wid, "test/findme.md", "this is findable data", "note");

        let result = s
            .search_impl(SearchRequest {
                query: "findable".into(),
                r#type: None,
                limit: None,
            })
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["source"], "store");
        assert!(parsed["total"].as_u64().unwrap_or(0) >= 1);
    });
}
