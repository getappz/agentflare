use crate::model::Session;
use crate::store::InsightsStore;

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub query: String,
    pub source: Option<String>,
    pub project: Option<String>,
    pub limit: usize,
    pub offset: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            source: None,
            project: None,
            limit: 20,
            offset: 0,
        }
    }
}

pub fn search(store: &InsightsStore, opts: &SearchOptions) -> anyhow::Result<Vec<Session>> {
    if opts.query.trim().is_empty() {
        return Ok(store.list_sessions(opts.limit, opts.offset)?);
    }
    // FTS5 query: escape quotes, support trigram
    let q = sanitize_fts_query(&opts.query);
    let mut results = store.search_sessions(&q, opts.limit + opts.offset)?;
    // apply source/project filters post-fts (small set)
    if let Some(src) = &opts.source {
        results.retain(|s| s.source.as_str() == src);
    }
    if let Some(proj) = &opts.project {
        results.retain(|s| s.project == *proj);
    }
    // paginate
    let start = opts.offset.min(results.len());
    let end = (start + opts.limit).min(results.len());
    Ok(results[start..end].to_vec())
}

fn sanitize_fts_query(q: &str) -> String {
    // wrap in quotes for phrase, fallback to OR of tokens
    let escaped = q.replace('"', "\"\"");
    if escaped.contains(' ') {
        format!("\"{}\"", escaped)
    } else {
        escaped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use chrono::Utc;
    #[test]
    fn sanitize() {
        assert_eq!(sanitize_fts_query("hello world"), "\"hello world\"");
        assert_eq!(sanitize_fts_query("hello"), "hello");
    }
    #[test]
    fn search_empty_returns_list() {
        let store = crate::store::InsightsStore::open_in_memory().unwrap();
        let s = Session {
            id: "s1".into(), source: SessionSource::ClaudeCode, project: "demo".into(),
            project_path: None, title: None, model: None, status: SessionStatus::Completed,
            awaiting_reason: None, started_at: None, updated_at: Utc::now(), ended_at: None,
            duration_secs: None,
            tokens: TokenUsage { input: 10, output: 5, cache_read: 0, cache_write: 0, reasoning: 0 },
            cost: None, turn_count: 1, tool_call_count: 0, subagent_count: 0,
            tags: vec![], starred: false, pid: None, cwd: None,
        };
        store.upsert_session(&s).unwrap();
        let opts = SearchOptions::default();
        let res = search(&store, &opts).unwrap();
        assert_eq!(res.len(), 1);
    }
}
