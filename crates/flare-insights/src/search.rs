use crate::model::Session;
use crate::store::InsightsStore;

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

// DRY: base search over turns FTS
pub fn search(store: &InsightsStore, opts: &SearchOptions) -> anyhow::Result<Vec<Session>> {
    if opts.query.trim().is_empty() {
        return Ok(store.list_sessions(opts.limit, opts.offset)?);
    }
    let q = sanitize_fts_query(&opts.query);
    let mut results = store.search_sessions(&q, opts.limit + opts.offset)?;

    // DRY: optionally extend with file_events/tool_calls matches (for opencode/claude)
    if opts.include_files || opts.include_tools {
        let extra = search_by_files_and_tools(store, &opts.query, opts.limit)?;
        for s in extra {
            if !results.iter().any(|r| r.id == s.id) {
                results.push(s);
            }
        }
        // re-sort by updated_at desc and limit
        results.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        results.truncate(opts.limit + opts.offset);
    }

    if let Some(src) = &opts.source {
        results.retain(|s| s.source.as_str() == src);
    }
    if let Some(proj) = &opts.project {
        results.retain(|s| s.project == *proj);
    }
    let start = opts.offset.min(results.len());
    let end = (start + opts.limit).min(results.len());
    Ok(results[start..end].to_vec())
}

fn search_by_files_and_tools(
    store: &InsightsStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<Session>> {
    let mut out = Vec::new();
    let q = format!("%{}%", query);
    // file_events path LIKE
    if let Ok(sessions) = store.search_sessions_by_file_path(&q, limit) {
        out.extend(sessions);
    }
    // tool_calls name LIKE
    if let Ok(sessions) = store.search_sessions_by_tool(&q, limit) {
        for s in sessions {
            if !out.iter().any(|r: &Session| r.id == s.id) {
                out.push(s);
            }
        }
    }
    Ok(out)
}

fn sanitize_fts_query(q: &str) -> String {
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
            id: "s1".into(),
            source: SessionSource::ClaudeCode,
            project: "demo".into(),
            project_path: None,
            title: None,
            model: None,
            status: SessionStatus::Completed,
            awaiting_reason: None,
            started_at: None,
            updated_at: Utc::now(),
            ended_at: None,
            duration_secs: None,
            tokens: TokenUsage {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: None,
            turn_count: 1,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: None,
        };
        store.upsert_session(&s).unwrap();
        let opts = SearchOptions::default();
        let res = search(&store, &opts).unwrap();
        assert_eq!(res.len(), 1);
    }
}
