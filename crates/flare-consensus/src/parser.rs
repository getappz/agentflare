//! Extractors for confidence markers and judge sections. Faithful port of
//! `ai-consensus-core`'s `parser.ts`.

use regex::Regex;
use std::sync::LazyLock;

fn clamp_confidence(n: Option<i64>) -> u8 {
    match n {
        None => 50,
        Some(n) => n.clamp(0, 100) as u8,
    }
}

/// Extract the trailing `CONFIDENCE: N` value. Clamps to \[0, 100\] and
/// defaults to 50 when the marker is absent or the following value is
/// malformed.
///
/// The default is intentional: an absent marker is a model compliance
/// issue, not a "low-confidence" signal, so it's treated as neutral rather
/// than letting it skew the consensus score downward.
///
/// Linear-time string parsing (no regex, no ReDoS).
pub fn extract_confidence(text: &str) -> u8 {
    extract_marker(text, "confidence:", false)
}

/// Extract the judge's self-reported synthesis confidence. Tolerates both
/// `JUDGE_CONFIDENCE: 87` and `JUDGE_CONFIDENCE: [87]` forms, since the
/// judge prompt wraps the placeholder in brackets.
pub fn extract_judge_confidence(text: &str) -> u8 {
    extract_marker(text, "judge_confidence:", true)
}

fn extract_marker(text: &str, prefix: &str, allow_bracket: bool) -> u8 {
    let lower = text.to_lowercase();
    let Some(idx) = lower.find(prefix) else {
        return clamp_confidence(None);
    };

    let bytes = text.as_bytes();
    let mut i = idx + prefix.len();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if allow_bracket && i < bytes.len() && bytes[i] == b'[' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
    }

    let start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }

    let n = text[start..i].parse::<i64>().ok();
    clamp_confidence(n)
}

/// Extract a named `## Heading`-style section from a judge synthesis.
/// Returns the trimmed section body, or "" if not found.
pub fn extract_judge_section(text: &str, heading: &str) -> String {
    let escaped = regex::escape(heading);
    let pattern = format!(r"(?is)##\s*{escaped}\s*\n(.*?)(?:\n##\s|\z)");
    let Ok(re) = Regex::new(&pattern) else {
        return String::new();
    };
    re.captures(text)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string())
        .unwrap_or_default()
}

static CONFIDENCE_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?is)\nCONFIDENCE:\s*\d+\s*\z").unwrap());

/// Strip the trailing `CONFIDENCE: N` line from a body. Used when quoting a
/// participant response back to a downstream model, so the marker from an
/// earlier round doesn't bleed into the next round's parser pass.
pub fn strip_confidence_line(text: &str) -> String {
    CONFIDENCE_LINE.replace(text, "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_confidence_standard() {
        assert_eq!(
            extract_confidence("Thoughtful analysis.\nCONFIDENCE: 87"),
            87
        );
    }

    #[test]
    fn extract_confidence_absent_defaults_to_50() {
        assert_eq!(extract_confidence("No marker at all."), 50);
        assert_eq!(extract_confidence(""), 50);
    }

    #[test]
    fn extract_confidence_malformed_defaults_to_50() {
        assert_eq!(extract_confidence("CONFIDENCE: not-a-number"), 50);
    }

    #[test]
    fn extract_confidence_case_insensitive() {
        assert_eq!(extract_confidence("confidence: 75"), 75);
        assert_eq!(extract_confidence("CoNfIdEnCe: 75"), 75);
    }

    #[test]
    fn extract_confidence_first_occurrence_wins() {
        assert_eq!(
            extract_confidence("CONFIDENCE: 30\nmore\nCONFIDENCE: 90"),
            30
        );
    }

    #[test]
    fn extract_confidence_clamps_above_100() {
        assert_eq!(extract_confidence("CONFIDENCE: 150"), 100);
        assert_eq!(extract_confidence("CONFIDENCE: 9999"), 100);
    }

    #[test]
    fn extract_confidence_boundaries() {
        assert_eq!(extract_confidence("CONFIDENCE: 0"), 0);
        assert_eq!(extract_confidence("CONFIDENCE: 100"), 100);
    }

    #[test]
    fn extract_confidence_extra_whitespace() {
        assert_eq!(extract_confidence("CONFIDENCE:    72"), 72);
        assert_eq!(extract_confidence("CONFIDENCE:\t72"), 72);
    }

    #[test]
    fn extract_confidence_surrounded_by_narrative() {
        assert_eq!(
            extract_confidence("…therefore CONFIDENCE: 65 (with caveats)"),
            65
        );
    }

    #[test]
    fn extract_judge_confidence_unbracketed() {
        assert_eq!(extract_judge_confidence("JUDGE_CONFIDENCE: 87"), 87);
    }

    #[test]
    fn extract_judge_confidence_bracketed() {
        assert_eq!(extract_judge_confidence("JUDGE_CONFIDENCE: [87]"), 87);
        assert_eq!(extract_judge_confidence("JUDGE_CONFIDENCE: [ 87 ]"), 87);
    }

    #[test]
    fn extract_judge_confidence_absent_defaults_to_50() {
        assert_eq!(extract_judge_confidence("no judge marker here"), 50);
    }

    #[test]
    fn extract_judge_confidence_case_insensitive() {
        assert_eq!(extract_judge_confidence("judge_confidence: 42"), 42);
    }

    #[test]
    fn extract_judge_confidence_clamps_above_100() {
        assert_eq!(extract_judge_confidence("JUDGE_CONFIDENCE: 250"), 100);
    }

    #[test]
    fn extract_judge_confidence_no_digits_defaults_to_50() {
        assert_eq!(
            extract_judge_confidence("JUDGE_CONFIDENCE: not-a-number"),
            50
        );
        assert_eq!(extract_judge_confidence("JUDGE_CONFIDENCE: [abc]"), 50);
    }

    const JUDGE_BODY: &str = "## Majority Position\nThey agree on X with qualifications.\n\n## Minority Positions\nAlice dissented on Y, citing cost.\n\n## Unresolved Disputes\n- Whether Z applies in edge case A\n\n## Synthesis Confidence\nJUDGE_CONFIDENCE: 82";

    #[test]
    fn extract_judge_section_up_to_next_heading() {
        assert_eq!(
            extract_judge_section(JUDGE_BODY, "Majority Position"),
            "They agree on X with qualifications."
        );
    }

    #[test]
    fn extract_judge_section_final_to_end_of_text() {
        assert_eq!(
            extract_judge_section(JUDGE_BODY, "Synthesis Confidence"),
            "JUDGE_CONFIDENCE: 82"
        );
    }

    #[test]
    fn extract_judge_section_missing_returns_empty() {
        assert_eq!(extract_judge_section(JUDGE_BODY, "Nonexistent Section"), "");
    }

    #[test]
    fn extract_judge_section_case_insensitive_heading() {
        assert_eq!(
            extract_judge_section(JUDGE_BODY, "majority POSITION"),
            "They agree on X with qualifications."
        );
    }

    #[test]
    fn extract_judge_section_escapes_regex_metacharacters() {
        let text = "## Section.A\nreal body\n\n## Section.B\nother\n";
        assert_eq!(extract_judge_section(text, "Section.A"), "real body");
        assert_eq!(
            extract_judge_section("## SectionXA\nbad\n", "Section.A"),
            ""
        );
    }

    #[test]
    fn extract_judge_section_trims_body() {
        let text = "## Heading\n\n   padded body   \n\n## Next\n";
        assert_eq!(extract_judge_section(text, "Heading"), "padded body");
    }

    #[test]
    fn strip_confidence_line_removes_trailing_marker() {
        assert_eq!(
            strip_confidence_line("body text\nCONFIDENCE: 80"),
            "body text"
        );
    }

    #[test]
    fn strip_confidence_line_no_marker_untouched() {
        assert_eq!(strip_confidence_line("just body"), "just body");
    }

    #[test]
    fn strip_confidence_line_case_insensitive() {
        assert_eq!(strip_confidence_line("body\nconfidence: 80"), "body");
    }

    #[test]
    fn strip_confidence_line_only_trailing_not_midbody() {
        let text = "This mentions CONFIDENCE: 50 in passing.\nActual body.\nCONFIDENCE: 80";
        assert_eq!(
            strip_confidence_line(text),
            "This mentions CONFIDENCE: 50 in passing.\nActual body."
        );
    }

    #[test]
    fn strip_confidence_line_tolerates_trailing_whitespace() {
        assert_eq!(strip_confidence_line("body\nCONFIDENCE: 80   "), "body");
    }
}
