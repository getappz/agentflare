/// Search mode: AND (every token must match) or OR (broader recall).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum MatchMode {
    /// AND semantics (default): every token must match.
    #[default]
    All,
    /// OR semantics: broader recall for retries.
    Any,
}

impl MatchMode {
    pub fn joiner(self) -> &'static str {
        match self {
            MatchMode::All => " AND ",
            MatchMode::Any => " OR ",
        }
    }
}

/// BM25 column weights for `bm25(table, w1, w2, ...)`.
///
/// Each position corresponds to an FTS5 column in schema order.
/// See [SQLite FTS5 bm25() docs](https://sqlite.org/fts5.html#the_bm25_function).
#[derive(Debug, Clone)]
pub struct Bm25Weights {
    pub weights: Vec<f64>,
}

impl Bm25Weights {
    pub const fn new(weights: Vec<f64>) -> Self {
        Bm25Weights { weights }
    }

    /// Render as comma-separated arguments for the `bm25()` SQL function.
    /// Returns empty string when no weights are set (uses default weights).
    pub fn sql_args(&self) -> String {
        if self.weights.is_empty() {
            String::new()
        } else {
            let mut s = String::with_capacity(self.weights.len() * 8);
            for (i, w) in self.weights.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push_str(&w.to_string());
            }
            s
        }
    }
}

/// Maximum allowed limit for any search query. Prevents negative `LIMIT`
/// in SQLite when a caller passes `usize::MAX`.
pub const MAX_LIMIT: usize = 1000;

/// Clamps `limit` to [`MAX_LIMIT`] and converts to `i64`, so the
/// `usize -> i64` cast can never produce a negative number.
pub fn clamped_limit(limit: usize) -> i64 {
    limit.min(MAX_LIMIT) as i64
}

/// Sanitize a free-text query into FTS5-safe double-quoted tokens.
///
/// Every whitespace-separated token is individually quoted so FTS5
/// operators (NEAR, *, OR, parentheses, column-prefix syntax) embedded
/// in user input cannot alter the query structure.
///
/// Returns `None` when the sanitized query is empty (e.g. all-punctuation
/// or whitespace-only input).
pub fn fts_query(query: &str, mode: MatchMode) -> Option<String> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|t| t.replace('"', ""))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    if tokens.is_empty() {
        return None;
    }
    Some(tokens.join(mode.joiner()))
}

/// Safer variant for single-table FTS5 where a bare `match` is used
/// without `AND/OR` between tokens (e.g. `observations_fts MATCH ?1`
/// where the query itself gets passed as a phrase). Wraps the full input.
pub fn fts_phrase_query(raw: &str) -> String {
    let tokens: Vec<String> = raw
        .split_whitespace()
        .map(|t| t.replace('"', ""))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    if tokens.is_empty() {
        // Every token sanitized to empty. Fall back to a safely escaped version.
        return format!("\"{}\"", raw.replace('"', "\"\""));
    }
    tokens.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_sanitizes_operators() {
        for q in [
            "foo\" OR \"bar",
            "NEAR(a b)",
            "symbol*",
            "(parentheses)",
            "col:val",
        ] {
            let result = fts_query(q, MatchMode::All);
            assert!(result.is_some(), "query {q:?} should not return None");
            let qs = result.unwrap();
            // Every whitespace-delimited piece became an individually
            // quoted token — no bare operator syntax remains.
            for tok in qs.split(" AND ") {
                assert!(
                    tok.starts_with('"') && tok.ends_with('"'),
                    "token {tok:?} in {qs:?} is not quoted"
                );
            }

            let result_any = fts_query(q, MatchMode::Any);
            assert!(result_any.is_some());
        }
    }

    #[test]
    fn empty_query_returns_none() {
        assert!(fts_query("  ", MatchMode::All).is_none());
        assert!(fts_query("", MatchMode::All).is_none());
        // Punctuation-only input still forms a token; returns Some
        assert!(fts_query("***", MatchMode::All).is_some());
    }

    #[test]
    fn all_mode_joins_with_and() {
        let q = fts_query("foo bar", MatchMode::All).unwrap();
        assert_eq!(q, "\"foo\" AND \"bar\"");
    }

    #[test]
    fn any_mode_joins_with_or() {
        let q = fts_query("foo bar", MatchMode::Any).unwrap();
        assert_eq!(q, "\"foo\" OR \"bar\"");
    }

    #[test]
    fn default_match_mode_is_all() {
        assert_eq!(MatchMode::default(), MatchMode::All);
    }

    #[test]
    fn fts_phrase_query_sanitizes_all_punctuation() {
        assert_eq!(fts_phrase_query("***"), "\"***\"");
    }

    #[test]
    fn fts_phrase_query_joins_with_space() {
        assert_eq!(fts_phrase_query("foo bar"), "\"foo\" \"bar\"");
    }

    #[test]
    fn clamped_limit_never_goes_negative() {
        assert_eq!(clamped_limit(usize::MAX), MAX_LIMIT as i64);
        assert_eq!(clamped_limit(5), 5);
    }

    #[test]
    fn bm25_weights_sql_args_renders_correctly() {
        let w = Bm25Weights::new(vec![3.0, 1.0, 2.0]);
        assert_eq!(w.sql_args(), "3,1,2");
    }

    #[test]
    fn bm25_weights_empty_returns_empty_string() {
        let w = Bm25Weights::new(vec![]);
        assert_eq!(w.sql_args(), "");
    }

    #[test]
    fn match_mode_joiner() {
        assert_eq!(MatchMode::All.joiner(), " AND ");
        assert_eq!(MatchMode::Any.joiner(), " OR ");
    }
}
