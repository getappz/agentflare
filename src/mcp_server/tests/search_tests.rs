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

fn search_sync(s: &AgentflareMcp, req: SearchRequest) -> Result<String, ErrorData> {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(s.search_impl(req))
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
    let err = search_sync(
        &s,
        SearchRequest {
            query: "".into(),
            r#type: None,
            limit: None,
            meta: None,
            path_glob: None,
            min_score: None,
            rerank: None,
            budget: None,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn search_store_returns_grouped_results() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let wid = ws_id(&s);

        seed_doc(
            &s,
            &wid,
            "docs/report.txt",
            "this is alpha content",
            "document",
        );
        seed_doc(
            &s,
            &wid,
            "item_attachment/item-1/memo.txt",
            "beta content here",
            "asset",
        );

        let result = search_sync(
            &s,
            SearchRequest {
                query: "alpha".into(),
                r#type: Some("store".into()),
                limit: Some(50),
                meta: None,
                path_glob: None,
                min_score: None,
                rerank: None,
                budget: None,
            },
        )
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
fn search_memory_requires_non_empty_query() {
    let (_tmp, s) = harness();
    let err = search_sync(
        &s,
        SearchRequest {
            query: "".into(),
            r#type: Some("memory".into()),
            limit: None,
            meta: None,
            path_glob: None,
            min_score: None,
            rerank: None,
            budget: None,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn search_memory_returns_grouped_observations() {
    crate::paths::test_support::with_temp_home(|| {
        // Seed an observation into brain.db
        let conn = crate::memory::store::open().unwrap();
        crate::memory::observations::save(
            &conn,
            None,
            "decision",
            "memory search test",
            "this is a test observation for memory search",
            None,
            Some("test-project"),
            None,
            None,
        )
        .unwrap();
        drop(conn);

        let (_tmp, s) = harness();
        let result = search_sync(
            &s,
            SearchRequest {
                query: "memory search".into(),
                r#type: Some("memory".into()),
                limit: Some(50),
                meta: None,
                path_glob: None,
                min_score: None,
                rerank: None,
                budget: None,
            },
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["source"], "memory");
        assert!(
            parsed["total"].as_u64().unwrap_or(0) >= 1,
            "expected >=1 result, got {result}"
        );
        let groups = parsed["groups"].as_object().unwrap();
        assert!(
            groups.contains_key("decision"),
            "expected decision group, got groups: {groups:?}"
        );
    });
}

#[test]
fn search_code_requires_non_empty_query() {
    let (_tmp, s) = harness();
    let err = search_sync(
        &s,
        SearchRequest {
            query: "".into(),
            r#type: Some("code".into()),
            limit: None,
            meta: None,
            path_glob: None,
            min_score: None,
            rerank: None,
            budget: None,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn search_code_returns_graceful_payload_via_gateway() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let result = search_sync(
            &s,
            SearchRequest {
                query: "search_impl".into(),
                r#type: Some("code".into()),
                limit: Some(10),
                meta: None,
                path_glob: None,
                min_score: None,
                rerank: None,
                budget: None,
            },
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["source"], "code");
        // With leanctx registered (auto-registration when the binary is
        // installed) this carries results; otherwise an error payload --
        // never an Err, never a panic.
        assert!(
            parsed.get("results").is_some() || parsed.get("error").is_some(),
            "expected results or error, got {parsed}"
        );
    });
}

#[test]
fn search_rejects_unknown_type() {
    let (_tmp, s) = harness();
    let err = search_sync(
        &s,
        SearchRequest {
            query: "x".into(),
            r#type: Some("bogus".into()),
            limit: None,
            meta: None,
            path_glob: None,
            min_score: None,
            rerank: None,
            budget: None,
        },
    )
    .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn search_store_defaults_to_store_type() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let wid = ws_id(&s);

        seed_doc(&s, &wid, "test/findme.md", "this is findable data", "note");

        let result = search_sync(
            &s,
            SearchRequest {
                query: "findable".into(),
                r#type: None,
                limit: None,
                meta: None,
                path_glob: None,
                min_score: None,
                rerank: None,
                budget: None,
            },
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["source"], "store");
        assert!(parsed["total"].as_u64().unwrap_or(0) >= 1);
    });
}

#[test]
fn search_store_includes_artifact_matches() {
    crate::paths::test_support::with_temp_home(|| {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            backend_db_override: Some(tmp.path().join("backend.db")),
            backend_project_link_override: Some(tmp.path().join("project.json")),
            artifacts_dir_override: Some(tmp.path().join("artifacts")),
            ..Default::default()
        };

        s.artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("handoff-notes".into()),
            content: Some("the search marker phrase lives here".into()),
            ..Default::default()
        }))
        .unwrap();

        let result = search_sync(
            &s,
            SearchRequest {
                query: "marker phrase".into(),
                r#type: None,
                limit: None,
                meta: None,
                path_glob: None,
                min_score: None,
                rerank: None,
                budget: None,
            },
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        let artifact_group = parsed["groups"]["artifact"].as_array();
        assert!(
            artifact_group.is_some_and(|a| !a.is_empty()),
            "artifact matches must appear in the store arm: {result}"
        );
    });
}

fn all_req(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.into(),
        r#type: Some("all".into()),
        limit: Some(20),
        meta: None,
        path_glob: None,
        min_score: None,
        rerank: None,
        budget: None,
    }
}

fn arm(refs: &[&str]) -> Vec<super::search_fuse::ArmHit> {
    refs.iter()
        .map(|r| super::search_fuse::ArmHit {
            r#ref: (*r).into(),
            title: (*r).into(),
            excerpt: String::new(),
        })
        .collect()
}

#[test]
fn fuse_rrf_orders_by_summed_reciprocal_rank_and_dedups() {
    use super::search_fuse::fuse_rrf;
    let hits = fuse_rrf(
        vec![
            ("store", arm(&["a", "shared", "b"])),
            ("memory", arm(&["shared", "c"])),
            ("code", arm(&["c", "a"])),
        ],
        10,
    );
    let refs: Vec<&str> = hits.iter().map(|h| h.r#ref.as_str()).collect();
    // shared = 1/62 + 1/61; a = 1/61 + 1/62 (tie, a seen first); c = 1/62 + 1/61 (tie)
    // b = 1/63 only.
    assert_eq!(refs.len(), 4, "shared ref must be one hit: {refs:?}");
    assert_eq!(refs.last(), Some(&"b"));
    let shared = hits.iter().find(|h| h.r#ref == "shared").unwrap();
    assert!((shared.score - (1.0 / 62.0 + 1.0 / 61.0)).abs() < 1e-12);
    // memory ranked it #1, store #2 -> memory's source wins
    assert_eq!(shared.source, "memory");
    let a = hits.iter().find(|h| h.r#ref == "a").unwrap();
    assert_eq!(a.source, "store");
    let c = hits.iter().find(|h| h.r#ref == "c").unwrap();
    assert_eq!(c.source, "code");
    assert_eq!(fuse_rrf(vec![("store", arm(&["a", "b", "c"]))], 2).len(), 2);
}

#[test]
fn opaque_hits_extracts_rank_order_from_text_and_json() {
    use super::search_fuse::opaque_hits;
    let text = serde_json::json!("src/a.rs:10: fn x\nsrc/b.rs:3: fn y");
    let refs: Vec<String> = opaque_hits(&text).into_iter().map(|h| h.r#ref).collect();
    assert_eq!(refs, ["src/a.rs:10", "src/b.rs:3"]);
    let wrapped = serde_json::json!({"content": [{"type": "text", "text": "[{\"url\":\"https://x\",\"title\":\"X\"},{\"url\":\"https://y\"}]"}]});
    let hits = opaque_hits(&wrapped);
    assert_eq!(hits[0].r#ref, "https://x");
    assert_eq!(hits[0].title, "X");
    assert_eq!(hits[1].r#ref, "https://y");
}

#[test]
fn search_all_fuses_store_and_memory() {
    crate::paths::test_support::with_temp_home(|| {
        let conn = crate::memory::store::open().unwrap();
        crate::memory::observations::save(
            &conn,
            None,
            "decision",
            "zebrafish decision",
            "zebrafish memory observation",
            None,
            Some("test-project"),
            None,
            None,
        )
        .unwrap();
        drop(conn);

        let (_tmp, s) = harness();
        let wid = ws_id(&s);
        seed_doc(
            &s,
            &wid,
            "docs/zebrafish.md",
            "zebrafish store document",
            "note",
        );

        let result = search_sync(&s, all_req("zebrafish")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["source"], "all");
        let hits = parsed["hits"].as_array().unwrap();
        let sources: Vec<&str> = hits.iter().map(|h| h["source"].as_str().unwrap()).collect();
        assert!(sources.contains(&"store"), "{result}");
        assert!(sources.contains(&"memory"), "{result}");
        // best-first, scores descending, and the doc carries the store ref
        let scores: Vec<f64> = hits.iter().map(|h| h["score"].as_f64().unwrap()).collect();
        assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
        assert!(
            hits.iter().any(|h| h["ref"] == "docs/zebrafish.md"),
            "{result}"
        );
        // web is off unless asked for
        assert!(!sources.contains(&"web"));
        assert!(
            !parsed["skipped"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a == "web")
        );
    });
}

#[test]
fn search_all_requires_non_empty_query() {
    let (_tmp, s) = harness();
    let err = search_sync(&s, all_req("")).unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn failing_delegated_arm_is_skipped_and_others_still_fuse() {
    use super::search_fuse::{delegated_arm, fuse_rrf};
    let failed = r#"{"source":"code","error":"leanctx ctx_search failed: down","results":[]}"#;
    assert!(delegated_arm(failed).is_none());
    let ok = delegated_arm(r#"{"source":"web","results":[{"url":"https://x"}]}"#).unwrap();
    let mut arms = vec![("store", arm(&["a"]))];
    arms.extend(delegated_arm(failed).map(|h| ("code", h)));
    arms.push(("web", ok));
    let hits = fuse_rrf(arms, 10);
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h.source != "code"));
}

#[test]
fn fuse_envelopes_dedups_real_store_arm_and_skips_failing_arms() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let wid = ws_id(&s);
        seed_doc(&s, &wid, "shared/doc.md", "quokka store document", "note");
        let store = search_sync(
            &s,
            SearchRequest {
                r#type: Some("store".into()),
                ..all_req("quokka")
            },
        )
        .unwrap();
        // code arm also ranks the same ref; memory arm is down; web errors
        let code = r#"{"source":"code","results":[{"path":"other.rs"},{"path":"shared/doc.md"}]}"#;
        let web = r#"{"source":"web","error":"rivalsearch web_search failed","results":[]}"#;
        let out = super::search_fuse::fuse_envelopes(
            "quokka",
            20,
            None,
            Some(&store),
            None,
            Some(code),
            Some(Some(web)),
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["skipped"], serde_json::json!(["memory", "web"]));
        let hits = v["hits"].as_array().unwrap();
        assert_eq!(hits.len(), 2, "shared ref must be one hit: {out}");
        // store rank 1 + code rank 2 beats code rank 1 alone
        assert_eq!(hits[0]["ref"], "shared/doc.md");
        assert_eq!(hits[0]["source"], "store");
        let want = 1.0 / 61.0 + 1.0 / 62.0;
        assert!((hits[0]["score"].as_f64().unwrap() - want).abs() < 1e-12);
        assert_eq!(hits[1]["ref"], "other.rs");
    });
}

#[test]
fn store_arm_orders_by_rank_not_raw_score() {
    // two doc types, so grouping loses order; the higher raw score is ranked second
    let store = r#"{"groups":{"a":[{"path":"first.md","score":0.1,"rank":0}],"b":[{"path":"second.md","score":9.0,"rank":1}]}}"#;
    let out = super::search_fuse::fuse_envelopes("q", 20, None, Some(store), None, None, None);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let hits = v["hits"].as_array().unwrap();
    assert_eq!(hits[0]["ref"], "first.md", "{out}");
    assert_eq!(hits[1]["ref"], "second.md", "{out}");
}

#[test]
fn budget_packs_by_score_with_citations_and_truncates_in_order() {
    // refs are 5 chars, no title/excerpt-less: cost = ceil((ref+title+excerpt)/4)
    let code = r#"{"source":"code","results":[
        {"path":"a.rs","snippet":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
        {"path":"huge.rs","snippet":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
        {"path":"c.rs","snippet":"cc"}]}"#;
    let run = |budget| {
        let out = super::search_fuse::fuse_envelopes("q", 20, budget, None, None, Some(code), None);
        serde_json::from_str::<serde_json::Value>(&out).unwrap()
    };
    // no budget: unchanged shape, all 3 hits, no citations
    let full = run(None);
    assert_eq!(full["hits"].as_array().unwrap().len(), 3);
    assert!(full.get("used_tokens").is_none());
    // a.rs = (4+4+40)/4 = 12 tokens, c.rs = (4+4+2)/4 = 3, huge.rs far larger
    let v = run(Some(16));
    let hits = v["hits"].as_array().unwrap();
    let refs: Vec<_> = hits.iter().map(|h| h["ref"].as_str().unwrap()).collect();
    assert_eq!(
        refs,
        ["a.rs", "c.rs"],
        "best first; oversized hit skipped: {v}"
    );
    assert!(v["used_tokens"].as_u64().unwrap() <= 16);
    assert_eq!(v["dropped"], 1);
    assert_eq!(hits[0]["citation"]["ref"], "a.rs");
    assert!(hits[0]["score"].as_f64().unwrap() > hits[1]["score"].as_f64().unwrap());
    // tinier budget: only the top-ranked hit survives
    let tiny = run(Some(12));
    let refs: Vec<_> = tiny["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["ref"].as_str().unwrap())
        .collect();
    assert_eq!(refs, ["a.rs"]);
    assert_eq!(run(Some(0))["total"], 0);
}

#[test]
fn budget_packs_before_capping_at_limit() {
    // the top-ranked hit is over budget; with limit=1 the pack must still
    // reach the lower-ranked hit that fits instead of coming back empty
    let code = r#"{"source":"code","results":[
        {"path":"huge.rs","snippet":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
        {"path":"c.rs","snippet":"cc"}]}"#;
    let out = super::search_fuse::fuse_envelopes("q", 1, Some(8), None, None, Some(code), None);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let refs: Vec<_> = v["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["ref"].as_str().unwrap())
        .collect();
    assert_eq!(refs, ["c.rs"], "{v}");
    assert_eq!(v["total"], 1);
}
