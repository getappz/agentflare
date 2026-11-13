use crate::store::InsightsStore;
use crate::store_query::{SearchFilters, SearchHit};

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: String,
    pub source: Option<String>,
    pub project: Option<String>,
    pub limit: usize,
    pub offset: usize,
    pub include_files: bool,
    pub include_tools: bool,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            source: None,
            project: None,
            limit: 20,
            offset: 0,
            include_files: true,
            include_tools: true,
        }
    }
}

/// Search turns and enabled retained evidence; one hit per matching session.
pub fn search(store: &InsightsStore, opts: &SearchOptions) -> anyhow::Result<Vec<SearchHit>> {
    if opts.query.trim().is_empty() {
        let sessions = store.list_sessions_filtered(
            opts.source.as_deref(),
            opts.project.as_deref(),
            opts.limit,
            opts.offset,
        )?;
        return Ok(sessions
            .into_iter()
            .map(|session| SearchHit {
                session,
                turn_seq: 0,
                rank: 0.0,
                snippet: String::new(),
            })
            .collect());
    }

    let f = SearchFilters {
        source: opts.source.clone(),
        project_prefix: opts.project.clone(),
        since: None,
        limit: opts.limit,
        offset: opts.offset,
        include_files: opts.include_files,
        include_tools: opts.include_tools,
    };
    Ok(store.search_hits(&opts.query, &f)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::store_query::SearchHit;
    use chrono::Utc;

    fn sample_session(id: &str, source: SessionSource, cwd: Option<&str>) -> Session {
        Session {
            id: id.into(),
            source,
            project: "demo".into(),
            project_path: cwd.map(str::to_string),
            title: None,
            model: None,
            status: SessionStatus::Completed,
            awaiting_reason: None,
            started_at: None,
            updated_at: Utc::now(),
            ended_at: None,
            duration_secs: None,
            tokens: TokenUsage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: None,
            cost_basis: CostBasis::Unknown,
            pricing_key: None,
            turn_count: 1,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: cwd.map(str::to_string),
        }
    }

    fn sample_turn(id: &str, session_id: &str, seq: u32, text: &str) -> Turn {
        Turn {
            id: id.into(),
            session_id: session_id.into(),
            seq,
            user_text: Some(text.into()),
            assistant_text: Some(text.into()),
            started_at: None,
            ended_at: None,
            tokens: None,
            cost_usd: None,
        }
    }

    #[test]
    fn retained_evidence_search_respects_flags_filters_and_pagination() {
        let store = InsightsStore::open_in_memory().unwrap();
        for (id, source, path) in [
            ("a", SessionSource::Codex, r"C:\Work\demo"),
            ("b", SessionSource::Codex, r"C:\Work\demo\child"),
            ("c", SessionSource::ClaudeCode, r"C:\Work\demo"),
            ("d", SessionSource::Codex, r"C:\Work\demo2"),
        ] {
            store
                .upsert_session(&sample_session(id, source, Some(path)))
                .unwrap();
            store.conn.execute("INSERT INTO file_events(id,session_id,path,kind,at) VALUES (?1,?1,'/needlemarker.rs','read','2026-01-01')", [id]).unwrap();
            store.conn.execute("INSERT INTO tool_calls(id,session_id,turn_seq,name,input,status) VALUES (?1,?1,3,'shell','{\"command\":\"needlemarker\"}','success')", [id]).unwrap();
        }
        store
            .conn
            .execute(
                "UPDATE sessions SET updated_at = '2026-01-01T00:00:00+00:00'",
                [],
            )
            .unwrap();
        let mut opts = SearchOptions {
            query: "needlemarker".into(),
            source: Some("codex".into()),
            project: Some("c:/work/demo/".into()),
            limit: 1,
            ..Default::default()
        };
        assert_eq!(search(&store, &opts).unwrap()[0].session.id, "a");
        opts.offset = 1;
        assert_eq!(search(&store, &opts).unwrap()[0].session.id, "b");
        opts.offset = 2;
        assert!(search(&store, &opts).unwrap().is_empty());
        opts.offset = 0;
        opts.include_files = false;
        assert_eq!(search(&store, &opts).unwrap()[0].turn_seq, 3);
        opts.include_tools = false;
        assert!(search(&store, &opts).unwrap().is_empty());
        opts.include_files = true;
        assert!(search(&store, &opts).unwrap()[0]
            .snippet
            .contains("needlemarker.rs"));
        opts.query = "xy".into();
        store
            .conn
            .execute("UPDATE file_events SET path='/xy.rs'", [])
            .unwrap();
        assert_eq!(search(&store, &opts).unwrap()[0].session.id, "a");
    }

    #[test]
    fn tool_evidence_search_includes_names_and_output() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("tool", SessionSource::Codex, None))
            .unwrap();
        store.conn.execute("INSERT INTO tool_calls(id,session_id,turn_seq,name,input,output,status) VALUES ('t','tool',7,'specialtool','{}','failureoutput','failed')", []).unwrap();
        for query in ["specialtool", "failureoutput"] {
            let opts = SearchOptions {
                query: query.into(),
                include_files: false,
                ..Default::default()
            };
            let hits = search(&store, &opts).unwrap();
            assert_eq!(hits.len(), 1, "{query}");
            assert_eq!(hits[0].turn_seq, 7);
            assert!(hits[0].snippet.contains(query));
        }
    }

    #[test]
    fn windows_project_paths_normalize_without_folding_unix_paths() {
        let store = InsightsStore::open_in_memory().unwrap();
        for (id, path) in [
            ("win", r"C:\Work\demo"),
            ("unix", "/Work/demo"),
            ("unc", r"\\server\share\demo"),
        ] {
            store
                .upsert_session(&sample_session(id, SessionSource::Codex, Some(path)))
                .unwrap();
        }
        for (prefix, expected) in [
            ("c:/work/demo", Some("win")),
            ("//SERVER/share/demo", Some("unc")),
            ("/work/demo", None),
        ] {
            let hits = store
                .list_sessions_filtered(None, Some(prefix), 20, 0)
                .unwrap();
            assert_eq!(hits.first().map(|s| s.id.as_str()), expected, "{prefix}");
        }
        let mut session = sample_session("project-path", SessionSource::Codex, Some("/else"));
        session.project_path = Some("C:/Work/demo/child".into());
        store.upsert_session(&session).unwrap();
        let hits = store
            .list_sessions_filtered(None, Some(r"c:\work\demo"), 20, 0)
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().any(|s| s.id == "project-path"));
    }

    #[test]
    fn tool_input_search_decodes_nested_json_strings() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("decoded", SessionSource::Codex, None))
            .unwrap();
        store.upsert_tool_call(&ToolCall {
            id: "decoded-tool".into(), session_id: "decoded".into(), turn_seq: 7,
            name: "shell".into(),
            input: serde_json::json!({"args": [{"path": r"C:\Work\demo", "quoted": "say \"hello\"", "tab": "tab\tvalue"}]}),
            output: None, status: "completed".into(), duration_ms: None, created_at: None,
        }).unwrap();
        for query in [r"C:\Work\demo", "say \"hello\"", "tab\tvalue"] {
            let hits = search(
                &store,
                &SearchOptions {
                    query: query.into(),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(hits.len(), 1, "{query:?}");
            assert_eq!(hits[0].turn_seq, 7);
            assert!(
                hits[0].snippet.contains(query),
                "{query:?}: {}",
                hits[0].snippet
            );
        }
    }

    #[test]
    fn retained_evidence_excerpts_include_late_matches_in_the_matching_field() {
        let store = InsightsStore::open_in_memory().unwrap();
        let needle = "needlemarker";
        for id in ["output", "input", "path"] {
            store
                .upsert_session(&sample_session(id, SessionSource::Codex, None))
                .unwrap();
        }
        for (id, input, output) in [
            (
                "output",
                serde_json::json!({"command": "input-prefix".repeat(100)}),
                Some(format!("{needle}{}", " tail".repeat(100))),
            ),
            (
                "input",
                serde_json::json!({"command": format!("{}{needle} context", "prefix ".repeat(100))}),
                None,
            ),
        ] {
            store
                .upsert_tool_call(&ToolCall {
                    id: id.into(),
                    session_id: id.into(),
                    turn_seq: 3,
                    name: "shell".into(),
                    input,
                    output,
                    status: "completed".into(),
                    duration_ms: None,
                    created_at: None,
                })
                .unwrap();
        }
        store
            .conn
            .execute(
                "INSERT INTO file_events(id, session_id, path, kind, at) VALUES ('p','path',?1,'read','2026-11-13T00:00:00Z')",
                [format!("C:/{}{needle}.rs", "路/".repeat(200))],
            )
            .unwrap();
        let hits = search(
            &store,
            &SearchOptions {
                query: needle.into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 3);
        for hit in hits {
            assert!(
                hit.snippet.contains(needle),
                "{}: {}",
                hit.session.id,
                hit.snippet
            );
            assert!(hit.snippet.chars().count() <= 240);
            if hit.session.id == "output" {
                assert!(hit.snippet.starts_with(needle), "{}", hit.snippet);
                assert!(!hit.snippet.contains("input-prefix"));
            }
        }
        let query = "N".repeat(210);
        store
            .conn
            .execute(
                "UPDATE file_events SET path=?1",
                [format!("{}{query}/tail", "前".repeat(200))],
            )
            .unwrap();
        let hits = search(
            &store,
            &SearchOptions {
                query: query.clone(),
                include_tools: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains(&query));
        assert!(hits[0].snippet.chars().count() <= 240);
    }

    #[test]
    fn tool_evidence_keeps_raw_and_cross_field_matching_and_tolerates_legacy_input() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("raw", SessionSource::Codex, None))
            .unwrap();
        store
            .upsert_tool_call(&ToolCall {
                id: "raw-tool".into(),
                session_id: "raw".into(),
                turn_seq: 1,
                name: "run".into(),
                input: serde_json::json!({"command": "cargo test"}),
                output: Some("finished okay".into()),
                status: "completed".into(),
                duration_ms: None,
                created_at: None,
            })
            .unwrap();
        for query in [r#"{"command":"cargo test"}"#, "run {", "} finished"] {
            let hits = search(
                &store,
                &SearchOptions {
                    query: query.into(),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(hits.len(), 1, "{query}");
            assert!(hits[0].snippet.contains(query));
        }
        store
            .conn
            .execute("UPDATE tool_calls SET input='not-json legacy-marker'", [])
            .unwrap();
        let hits = search(
            &store,
            &SearchOptions {
                query: "legacy-marker".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn sanitize() {
        assert_eq!(
            flare_search_kit::fts_phrase_query("hello world"),
            "\"hello\" \"world\""
        );
        assert_eq!(flare_search_kit::fts_phrase_query("hello"), "\"hello\"");
        assert_eq!(flare_search_kit::fts_phrase_query("gpt-4"), "\"gpt-4\"");
    }

    #[test]
    fn search_empty_returns_list() {
        let store = crate::store::InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("s1", SessionSource::ClaudeCode, None))
            .unwrap();
        let opts = SearchOptions::default();
        let res = search(&store, &opts).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].session.id, "s1");
        assert_eq!(res[0].turn_seq, 0);
        assert_eq!(res[0].rank, 0.0);
        assert!(res[0].snippet.is_empty());
    }

    #[test]
    fn project_filter_survives_limit_smaller_than_other_project_hits() {
        let store = InsightsStore::open_in_memory().unwrap();
        let marker = "needlemarker";
        for i in 0..50 {
            let id = format!("a{i}");
            store
                .upsert_session(&sample_session(&id, SessionSource::Codex, Some("/proj/a")))
                .unwrap();
            store
                .upsert_turn(&sample_turn(&format!("t-{id}"), &id, 1, marker))
                .unwrap();
        }
        for i in 0..5 {
            let id = format!("b{i}");
            store
                .upsert_session(&sample_session(&id, SessionSource::Codex, Some("/proj/b")))
                .unwrap();
            store
                .upsert_turn(&sample_turn(&format!("t-{id}"), &id, 1, marker))
                .unwrap();
        }
        let opts = SearchOptions {
            query: marker.into(),
            project: Some("/proj/b".into()),
            limit: 10,
            ..Default::default()
        };
        let res = search(&store, &opts).unwrap();
        assert_eq!(res.len(), 5);
        let mut ids: Vec<_> = res.iter().map(|h| h.session.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 5);
        for h in &res {
            assert!(
                h.session.cwd.as_deref() == Some("/proj/b")
                    || h.session.project_path.as_deref() == Some("/proj/b")
            );
        }
    }

    #[test]
    fn one_hit_per_session_even_when_many_turns_match() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("multi", SessionSource::ClaudeCode, None))
            .unwrap();
        for seq in 1..=8 {
            store
                .upsert_turn(&sample_turn(
                    &format!("t{seq}"),
                    "multi",
                    seq,
                    "sharedphrase content",
                ))
                .unwrap();
        }
        let opts = SearchOptions {
            query: "sharedphrase".into(),
            limit: 20,
            ..Default::default()
        };
        let res = search(&store, &opts).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].session.id, "multi");
        assert!(res[0].turn_seq >= 1);
        assert!(!res[0].snippet.is_empty());
    }

    #[test]
    fn empty_query_honours_source_filter() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("c1", SessionSource::ClaudeCode, None))
            .unwrap();
        store
            .upsert_session(&sample_session("x1", SessionSource::Codex, None))
            .unwrap();
        let opts = SearchOptions {
            query: String::new(),
            source: Some("claude_code".into()),
            limit: 10,
            ..Default::default()
        };
        let res = search(&store, &opts).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].session.id, "c1");
        assert_eq!(res[0].turn_seq, 0);
        assert_eq!(res[0].rank, 0.0);
        assert!(res[0].snippet.is_empty());
    }

    #[test]
    fn two_char_query_uses_like_fallback() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("sxy", SessionSource::Codex, None))
            .unwrap();
        store
            .upsert_turn(&sample_turn("t1", "sxy", 1, "find xy here"))
            .unwrap();
        store
            .upsert_session(&sample_session("sno", SessionSource::Codex, None))
            .unwrap();
        store
            .upsert_turn(&sample_turn("t2", "sno", 1, "nothing relevant"))
            .unwrap();
        let opts = SearchOptions {
            query: "xy".into(),
            limit: 10,
            ..Default::default()
        };
        let res = search(&store, &opts).unwrap();
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].session.id, "sxy");
        let _hit: &SearchHit = &res[0];
    }

    #[test]
    fn snippet_comes_from_the_column_that_matched() {
        let store = InsightsStore::open_in_memory().unwrap();
        store
            .upsert_session(&sample_session("u", SessionSource::Codex, None))
            .unwrap();
        let mut turn = sample_turn("t1", "u", 1, "needleword in the question");
        turn.assistant_text = Some("an unrelated reply".into());
        store.upsert_turn(&turn).unwrap();
        let opts = SearchOptions {
            query: "needleword".into(),
            limit: 10,
            ..Default::default()
        };
        let res = search(&store, &opts).unwrap();
        assert_eq!(res.len(), 1);
        assert!(
            res[0].snippet.contains("[needleword]"),
            "{}",
            res[0].snippet
        );
    }

    #[test]
    fn tied_hits_page_in_a_stable_order() {
        for (query, text) in [("zq", "zq short"), ("tiedphrase", "tiedphrase text")] {
            let store = InsightsStore::open_in_memory().unwrap();
            for id in ["c", "a", "b"] {
                store
                    .upsert_session(&sample_session(id, SessionSource::Codex, None))
                    .unwrap();
                store
                    .upsert_turn(&sample_turn(&format!("t{id}"), id, 1, text))
                    .unwrap();
            }
            store
                .conn
                .execute(
                    "UPDATE sessions SET updated_at = '2026-01-01T00:00:00+00:00'",
                    [],
                )
                .unwrap();
            let paged: Vec<String> = (0..3)
                .map(|offset| {
                    let opts = SearchOptions {
                        query: query.into(),
                        limit: 1,
                        offset,
                        ..Default::default()
                    };
                    search(&store, &opts).unwrap()[0].session.id.clone()
                })
                .collect();
            assert_eq!(paged, ["a", "b", "c"], "query {query}");
        }
    }
}
