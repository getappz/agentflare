//! BM25 search over the FTS5 tools index. Same query-sanitization approach
//! as `crates/skill-registry/src/search.rs`: every whitespace token is
//! double-quoted so FTS5 operators embedded in free-text queries can't
//! alter the query.

use rusqlite::Connection;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    /// AND semantics (default): every token must match.
    All,
    /// OR semantics: broader recall for retries.
    Any,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolHit {
    pub server: String,
    pub tool: String,
    pub description: String,
    pub input_schema: Value,
    pub score: f64,
}

/// Ceiling applied to a caller-supplied `limit` before it's used in
/// SQLite's `LIMIT` clause. Two reasons: (1) casting a huge `usize` (e.g.
/// `usize::MAX`, reachable via `gateway_search`'s MCP request) straight to
/// `i64` can wrap around to a negative number in two's-complement, and
/// SQLite treats a negative `LIMIT` as "no limit" — silently defeating the
/// cap; (2) even ignoring the cast, tool search results are meant to be a
/// short top-K list in this crate's v1 usage, not an unbounded dump.
pub const MAX_LIMIT: usize = 1000;

/// Clamps `limit` to [`MAX_LIMIT`] and converts to `i64` for the SQLite
/// `LIMIT` clause, so the `usize -> i64` cast can never produce a negative
/// number regardless of what a caller supplies.
fn clamped_limit(limit: usize) -> i64 {
    limit.min(MAX_LIMIT) as i64
}

fn fts_query(query: &str, mode: MatchMode) -> Option<String> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|t| t.replace('"', ""))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let joiner = match mode {
        MatchMode::All => " AND ",
        MatchMode::Any => " OR ",
    };
    Some(tokens.join(joiner))
}

pub fn search(
    conn: &Connection,
    query: &str,
    limit: usize,
    mode: MatchMode,
) -> rusqlite::Result<Vec<ToolHit>> {
    let Some(fts) = fts_query(query, mode) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT t.server, t.name, t.description, t.input_schema,
                bm25(tools_fts, 2.0, 3.0, 1.0) AS score
         FROM tools_fts
         JOIN tools t ON t.rowid = tools_fts.rowid
         WHERE tools_fts MATCH ?1
         ORDER BY score
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![fts, clamped_limit(limit)], |r| {
        let schema_json: String = r.get(3)?;
        let input_schema: Value = serde_json::from_str(&schema_json).unwrap_or(Value::Null);
        Ok(ToolHit {
            server: r.get(0)?,
            tool: r.get(1)?,
            description: r.get(2)?,
            input_schema,
            score: r.get(4)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ServerTools, open_in_memory, rebuild};
    use crate::types::ToolEntry;

    fn seed() -> Connection {
        let mut conn = open_in_memory().unwrap();
        let mk = |name: &str, desc: &str| ToolEntry {
            name: name.into(),
            description: desc.into(),
            input_schema: serde_json::json!({}),
        };
        rebuild(
            &mut conn,
            &[
                ServerTools {
                    server: "narsil".into(),
                    tools: vec![
                        mk("find_symbols", "Search for symbol definitions by pattern"),
                        mk("references", "Find all references to a symbol"),
                    ],
                },
                ServerTools {
                    server: "github".into(),
                    tools: vec![mk("list_issues", "List open issues for a repository")],
                },
            ],
        )
        .unwrap();
        conn
    }

    #[test]
    fn all_mode_requires_every_token() {
        let conn = seed();
        let hits = search(&conn, "symbol references", 5, MatchMode::All).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tool, "references");
    }

    #[test]
    fn any_mode_broadens_recall() {
        let conn = seed();
        let hits = search(&conn, "symbol issues", 5, MatchMode::Any).unwrap();
        let tools: Vec<_> = hits.iter().map(|h| h.tool.as_str()).collect();
        assert!(tools.contains(&"find_symbols"));
        assert!(tools.contains(&"references"));
        assert!(tools.contains(&"list_issues"));
    }

    #[test]
    fn server_field_is_preserved() {
        let conn = seed();
        let hits = search(&conn, "issues", 5, MatchMode::Any).unwrap();
        assert_eq!(hits[0].server, "github");
    }

    #[test]
    fn fts_operators_in_query_are_neutralized() {
        let conn = seed();
        for q in [
            "symbol\" OR \"x",
            "NEAR(a b)",
            "issues*",
            "(references)",
            "col:val",
        ] {
            search(&conn, q, 5, MatchMode::Any).unwrap();
        }
    }

    #[test]
    fn empty_query_returns_empty() {
        let conn = seed();
        assert!(search(&conn, "  ", 5, MatchMode::All).unwrap().is_empty());
    }

    #[test]
    fn clamped_limit_never_goes_negative_for_a_huge_input() {
        // usize::MAX cast straight to i64 would be -1 — SQLite treats a
        // negative LIMIT as "no limit", silently defeating the cap.
        let clamped = clamped_limit(usize::MAX);
        assert!(clamped > 0, "clamped limit went non-positive: {clamped}");
        assert_eq!(clamped, MAX_LIMIT as i64);
    }

    #[test]
    fn clamped_limit_leaves_small_values_untouched() {
        assert_eq!(clamped_limit(5), 5);
    }

    #[test]
    fn search_with_a_huge_limit_does_not_panic_and_still_returns_results() {
        let conn = seed();
        let hits = search(&conn, "symbol references", usize::MAX, MatchMode::All).unwrap();
        assert_eq!(hits.len(), 1);
    }
}
