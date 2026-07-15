//! Flare context compression layer — session transcript compaction.
//! Ephemeral FTS5/BM25 line scorer for transcript relevance ranking.
//! Merged from compact.rs. Used by the PreCompact hook.
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
pub fn score_lines(lines: &[LineEntry], query: &str) -> rusqlite::Result<Vec<ScoredLine>> {
    if lines.is_empty() {
        return Ok(vec![]);
    }

    let Some(safe_query) = fts_query(query, MatchMode::Any) else {
        return Ok(vec![]);
    };

    let conn = Connection::open_in_memory()?;

    conn.execute_batch("CREATE VIRTUAL TABLE transcript_fts USING fts5(\"ix\" UNINDEXED, text);")?;

    {
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt =
                tx.prepare("INSERT INTO transcript_fts(\"ix\", text) VALUES(?1, ?2);")?;
            for line in lines {
                stmt.execute(rusqlite::params![line.index as i64, &line.text])?;
            }
        }
        tx.commit()?;
    }

    let weights = Bm25Weights::new(vec![]);
    let sql = format!(
        "SELECT \"ix\", text, bm25(transcript_fts{}) AS score FROM transcript_fts WHERE transcript_fts MATCH ?1 ORDER BY score, \"ix\"",
        weights.sql_args()
    );

    let mut stmt = conn.prepare(&sql)?;

    let results = stmt.query_map(rusqlite::params![safe_query], |row| {
        Ok(ScoredLine {
            index: row.get::<_, i64>("ix")? as usize,
            text: row.get("text")?,
            score: row.get("score")?,
        })
    })?;

    results.collect()
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
        let scored = score_lines(&lines, "database query").unwrap();
        assert!(!scored.is_empty());
        let matched: Vec<usize> = scored.iter().map(|s| s.index).collect();
        assert!(matched.contains(&0));
        assert!(matched.contains(&2));
        assert!(!matched.contains(&1));
    }

    #[test]
    fn score_lines_returns_empty_for_no_match() {
        let lines = vec![LineEntry {
            index: 0,
            text: "Fix the login button color.".to_string(),
        }];
        let scored = score_lines(&lines, "quantum physics").unwrap();
        assert!(scored.is_empty());
    }

    #[test]
    fn score_lines_returns_empty_for_empty_lines() {
        let scored = score_lines(&[], "anything").unwrap();
        assert!(scored.is_empty());
    }

    #[test]
    fn score_lines_returns_empty_for_empty_query() {
        let lines = vec![LineEntry {
            index: 0,
            text: "Hello world.".to_string(),
        }];
        let scored = score_lines(&lines, "").unwrap();
        assert!(scored.is_empty());
    }

    #[test]
    fn score_lines_returns_empty_for_punctuation_only_query() {
        let lines = vec![LineEntry {
            index: 0,
            text: "Hello world.".to_string(),
        }];
        let scored = score_lines(&lines, "***").unwrap();
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
        let scored = score_lines(&lines, "cargo build").unwrap();
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
        let scored = score_lines(&lines, "database").unwrap();
        assert_eq!(scored.len(), 2);
        assert_eq!(scored[0].index, 0);
        assert_eq!(scored[1].index, 1);
    }
}
