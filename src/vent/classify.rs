use regex::Regex;
use std::sync::LazyLock;

pub const ACTIONABLE_SEEN_THRESHOLD: i64 = 3;

static MARKER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(broke|broken|fails?|failing|wrong|should|missing|can'?t|cannot|error|panic|crash|hang|stuck|nonexistent|fabricat\w*)\b|[\w./-]+\.\w{1,6}",
    )
    .expect("static marker regex is valid")
});

pub fn topic_key(message: &str) -> String {
    message
        .to_lowercase()
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn severity_rank(severity: &str) -> u8 {
    match severity {
        "high" => 2,
        "medium" => 1,
        _ => 0,
    }
}

/// Normalize user-supplied severity to exactly "low" | "medium" | "high"
/// (case-insensitive), defaulting to "medium" for anything else. Shared by
/// the MCP `vent` tool and the `agentflare vent say` CLI so both entry
/// points classify identically.
pub fn normalize_severity(input: Option<&str>) -> &'static str {
    match input.map(str::to_lowercase).as_deref() {
        Some("low") => "low",
        Some("high") => "high",
        _ => "medium",
    }
}

pub fn classify(severity: &str, seen_count: i64, message: &str) -> bool {
    severity == "high" || seen_count >= ACTIONABLE_SEEN_THRESHOLD || MARKER_RE.is_match(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_key_is_stable_across_case_ws_punct() {
        assert_eq!(topic_key("Disk  FULL!!"), topic_key("disk full"));
        assert_eq!(topic_key("  a,b.  c "), "ab c");
    }

    #[test]
    fn classify_truth_table() {
        assert!(classify("high", 1, "all good"));
        assert!(!classify("low", 2, "all good"));
        assert!(classify("low", 3, "all good"));
        assert!(classify("low", 1, "the build fails on windows"));
        assert!(classify("low", 1, "I fabricated $CLAUDE_JOB_DIR"));
        assert!(classify("low", 1, "cannot open config.toml"));
        assert!(!classify("low", 1, "this is a normal note"));
    }

    #[test]
    fn severity_rank_orders_low_medium_high() {
        assert!(severity_rank("high") > severity_rank("medium"));
        assert!(severity_rank("medium") > severity_rank("low"));
        assert_eq!(severity_rank("garbage"), severity_rank("low"));
    }

    #[test]
    fn normalize_severity_is_case_insensitive_and_defaults_to_medium() {
        assert_eq!(normalize_severity(Some("High")), "high");
        assert_eq!(normalize_severity(Some("LOW")), "low");
        assert_eq!(normalize_severity(Some("garbage")), "medium");
        assert_eq!(normalize_severity(None), "medium");
    }
}
