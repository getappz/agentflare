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

/// Search turns; returns one [`SearchHit`] per matching session.
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
