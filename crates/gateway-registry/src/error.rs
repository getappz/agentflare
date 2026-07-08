//! Typed gateway errors and a small hand-rolled Levenshtein-distance fuzzy
//! matcher for "did you mean" suggestions on unknown server/tool names.
//! Written fresh — not copied from forgemax's `forge-error` (FSL-licensed).

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("{0}")]
    ServerNotFound(String),
    #[error("{0}")]
    ToolNotFound(String),
    #[error("backend kind '{0}' is not implemented yet")]
    NotImplemented(String),
    #[error("downstream connection failed: {0}")]
    Connection(String),
    #[error("downstream call failed: {0}")]
    Upstream(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// Levenshtein edit distance between two strings (character-wise).
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (la, lb) = (a.len(), b.len());
    let mut dp = vec![vec![0usize; lb + 1]; la + 1];
    for (i, row) in dp.iter_mut().enumerate().take(la + 1) {
        row[0] = i;
    }
    for j in 0..=lb {
        dp[0][j] = j;
    }
    for i in 1..=la {
        for j in 1..=lb {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1).min(dp[i][j - 1] + 1).min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[la][lb]
}

/// The closest candidate to `target` by edit distance, if any candidate is
/// within a distance of 3 (roughly "one typo or two" for short tool names).
pub fn suggest(target: &str, candidates: &[String]) -> Option<String> {
    candidates
        .iter()
        .map(|c| (c, levenshtein(target, c)))
        .filter(|(_, d)| *d <= 3)
        .min_by_key(|(_, d)| *d)
        .map(|(c, _)| c.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_identical_is_zero() {
        assert_eq!(levenshtein("find_symbols", "find_symbols"), 0);
    }

    #[test]
    fn levenshtein_one_typo() {
        assert_eq!(levenshtein("find_symbls", "find_symbols"), 1);
    }

    #[test]
    fn suggest_picks_closest_within_threshold() {
        let candidates = vec!["find_symbols".to_string(), "list_issues".to_string()];
        assert_eq!(suggest("find_symbls", &candidates), Some("find_symbols".to_string()));
    }

    #[test]
    fn suggest_returns_none_when_too_far() {
        let candidates = vec!["find_symbols".to_string()];
        assert_eq!(suggest("completely_unrelated_name", &candidates), None);
    }

    #[test]
    fn suggest_returns_none_for_empty_candidates() {
        assert_eq!(suggest("anything", &[]), None);
    }
}
