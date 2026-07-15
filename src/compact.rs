//! Ephemeral FTS5/BM25 line scorer for session transcript compaction.
//! Creates an in-memory SQLite FTS5 table per call and ranks lines by
//! relevance to a query. Used by the PreCompact hook (Phase 2) to
//! identify which transcript lines to keep during compaction.
use flare_search_kit::{Bm25Weights, MatchMode, fts_query};
use rusqlite::Connection;

/// A single line from a session transcript.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct LineEntry {
    pub index: usize,
    pub text: String,
}

/// A transcript line with its BM25 relevance score.
/// Lower scores = more relevant (0 = perfect match).
#[derive(Debug, Clone, serde::Serialize)]
#[allow(dead_code)]
pub struct ScoredLine {
    pub index: usize,
    pub text: String,
    pub score: f64,
}

/// Score transcript lines by BM25 relevance to a query.
///
/// Creates an ephemeral in-memory FTS5 table, inserts each line as a row,
/// and returns matched lines ranked by relevance (most relevant first).
/// Returns empty vec when:
/// - `lines` is empty
/// - `query` sanitizes to nothing (whitespace, punctuation-only)
/// - no lines match the query
pub fn score_lines(lines: &[LineEntry], query: &str) -> Vec<ScoredLine> {
    if lines.is_empty() {
        return vec![];
    }

    // OR mode for broader recall — compaction keeps all potentially relevant lines.
    let Some(safe_query) = fts_query(query, MatchMode::Any) else {
        return vec![];
    };

    let conn = Connection::open_in_memory().expect("in-memory SQLite connection");

    conn.execute_batch("CREATE VIRTUAL TABLE transcript_fts USING fts5(\"ix\" UNINDEXED, text);")
        .expect("create FTS5 table");

    {
        let tx = conn.unchecked_transaction().expect("start transaction");
        {
            let mut stmt = tx
                .prepare("INSERT INTO transcript_fts(\"ix\", text) VALUES(?1, ?2);")
                .expect("prepare insert");
            for line in lines {
                stmt.execute(rusqlite::params![line.index as i64, &line.text])
                    .expect("insert line");
            }
        }
        tx.commit().expect("commit transcript insert batch");
    }

    let weights = Bm25Weights::new(vec![]);
    let sql = format!(
        "SELECT \"ix\", text, bm25(transcript_fts{}) AS score \
         FROM transcript_fts \
         WHERE transcript_fts MATCH ?1 \
         ORDER BY score, \"ix\"",
        weights.sql_args()
    );

    let mut stmt = conn.prepare(&sql).expect("prepare query");

    let results = stmt
        .query_map(rusqlite::params![safe_query], |row| {
            Ok(ScoredLine {
                index: row.get::<_, i64>("ix")? as usize,
                text: row.get("text")?,
                score: row.get("score")?,
            })
        })
        .expect("query lines");

    results.filter_map(|r| r.ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_lines_matches_in_or_mode() {
        let lines = vec![
            LineEntry {
                index: 0,
                text: "Let me check the database schema.".to_string(),
            },
            LineEntry {
                index: 1,
                text: "The weather is nice today.".to_string(),
            },
            LineEntry {
                index: 2,
                text: "I need to query the users table.".to_string(),
            },
        ];
        let scored = score_lines(&lines, "database query");
        assert!(!scored.is_empty(), "should match at least one line");
        let matched: Vec<usize> = scored.iter().map(|s| s.index).collect();
        assert!(matched.contains(&0), "line with 'database' should match");
        assert!(matched.contains(&2), "line with 'query' should match");
        assert!(!matched.contains(&1), "unrelated line should not match");
        // BM25 lower score = more relevant; the line with more matching
        // terms should rank higher (index 2 has 'query', index 0 has 'database')
        // Both match one term each, so scores should be comparable.
    }

    #[test]
    fn score_lines_returns_empty_for_no_match() {
        let lines = vec![LineEntry {
            index: 0,
            text: "Fix the login button color.".to_string(),
        }];
        let scored = score_lines(&lines, "quantum physics");
        assert!(scored.is_empty());
    }

    #[test]
    fn score_lines_returns_empty_for_empty_lines() {
        let scored = score_lines(&[], "anything");
        assert!(scored.is_empty());
    }

    #[test]
    fn score_lines_returns_empty_for_empty_query() {
        let lines = vec![LineEntry {
            index: 0,
            text: "Hello world.".to_string(),
        }];
        let scored = score_lines(&lines, "");
        assert!(scored.is_empty());
    }

    #[test]
    fn score_lines_returns_empty_for_punctuation_only_query() {
        let lines = vec![LineEntry {
            index: 0,
            text: "Hello world.".to_string(),
        }];
        let scored = score_lines(&lines, "***");
        assert!(scored.is_empty());
    }

    #[test]
    fn score_lines_handles_special_characters() {
        let lines = vec![
            LineEntry {
                index: 0,
                text: "Run npm install && cargo build --release.".to_string(),
            },
            LineEntry {
                index: 1,
                text: "The price is $19.99 + tax (10%).".to_string(),
            },
        ];
        let scored = score_lines(&lines, "cargo build");
        assert_eq!(scored.len(), 1);
        assert_eq!(scored[0].index, 0);
    }

    #[test]
    fn score_lines_preserves_line_order_within_same_score() {
        let lines = vec![
            LineEntry {
                index: 0,
                text: "database schema".to_string(),
            },
            LineEntry {
                index: 1,
                text: "database schema".to_string(),
            },
        ];
        let scored = score_lines(&lines, "database");
        assert_eq!(scored.len(), 2);
        assert_eq!(scored[0].index, 0);
        assert_eq!(scored[1].index, 1);
    }
}
