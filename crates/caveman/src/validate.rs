//! Structural preservation checks: headings, code blocks, URLs, and inline
//! code must be preserved exactly. A fast mechanical net — NOT a substitute
//! for semantic review of what changed (that's a human/subagent's job).

use regex::Regex;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::LazyLock;

static URL_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://[^\s)]+").unwrap());
static FENCE_OPEN_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\s{0,3})(`{3,}|~{3,})(.*)$").unwrap());
static HEADING_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^(#{1,6})\s+(.*)").unwrap());
static INLINE_CODE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`]+)`").unwrap());

fn extract_headings(text: &str) -> Vec<(String, String)> {
    HEADING_REGEX
        .captures_iter(text)
        .map(|c| (c[1].to_string(), c[2].trim().to_string()))
        .collect()
}

/// Line-based fenced code block extractor. Handles ``` and ~~~ fences with
/// variable length (CommonMark: closing fence must use the same character
/// and be at least as long as the opening one) — including nested fences,
/// e.g. an outer 4-backtick block wrapping inner 3-backtick content.
fn extract_code_blocks(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(caps) = FENCE_OPEN_REGEX.captures(lines[i]) else {
            i += 1;
            continue;
        };
        let fence_char = caps[2].chars().next().unwrap();
        let fence_len = caps[2].len();
        let mut block_lines = vec![lines[i]];
        i += 1;
        let mut closed = false;
        while i < lines.len() {
            if let Some(close_caps) = FENCE_OPEN_REGEX.captures(lines[i]) {
                if close_caps[2].chars().next() == Some(fence_char)
                    && close_caps[2].len() >= fence_len
                    && close_caps[3].trim().is_empty()
                {
                    block_lines.push(lines[i]);
                    closed = true;
                    i += 1;
                    break;
                }
            }
            block_lines.push(lines[i]);
            i += 1;
        }
        if closed {
            blocks.push(block_lines.join("\n"));
        }
        // Unclosed fences are silently skipped — malformed markdown,
        // including them would cause false-positive validation failures.
    }
    blocks
}

fn extract_urls(text: &str) -> HashSet<String> {
    URL_REGEX.find_iter(text).map(|m| m.as_str().to_string()).collect()
}

fn extract_inline_codes(text: &str) -> Vec<String> {
    // Strip fenced blocks first so inline `code` inside a fenced example
    // isn't double-counted.
    let mut without_fences = text.to_string();
    for block in extract_code_blocks(text) {
        without_fences = without_fences.replacen(&block, "", 1);
    }
    INLINE_CODE_REGEX
        .captures_iter(&without_fences)
        .map(|c| c[1].to_string())
        .collect()
}

fn counts(codes: Vec<String>) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for code in codes {
        *map.entry(code).or_insert(0) += 1;
    }
    map
}

pub fn validate(orig: &str, comp: &str) -> Vec<String> {
    let mut errors = Vec::new();

    let h1 = extract_headings(orig);
    let h2 = extract_headings(comp);
    if h1.len() != h2.len() {
        errors.push(format!("Heading count mismatch: {} vs {}", h1.len(), h2.len()));
    }

    if extract_code_blocks(orig) != extract_code_blocks(comp) {
        errors.push("Code blocks not preserved exactly".to_string());
    }

    let u1 = extract_urls(orig);
    let u2 = extract_urls(comp);
    if u1 != u2 {
        let lost: Vec<_> = u1.difference(&u2).collect();
        let added: Vec<_> = u2.difference(&u1).collect();
        errors.push(format!("URL mismatch: lost={lost:?}, added={added:?}"));
    }

    let c1 = counts(extract_inline_codes(orig));
    let c2 = counts(extract_inline_codes(comp));
    if c1 != c2 {
        let lost: Vec<String> = c1
            .iter()
            .filter(|(code, count)| c2.get(*code).copied().unwrap_or(0) < **count)
            .map(|(code, count)| {
                let remaining = c2.get(code).copied().unwrap_or(0);
                format!("{code} (lost {} of {count} occurrences)", count - remaining)
            })
            .collect();
        if !lost.is_empty() {
            errors.push(format!("Inline code lost: {lost:?}"));
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_passes() {
        let text = "# Title\n\nSome `code` and https://x.com\n\n```py\nprint(1)\n```\n";
        assert!(validate(text, text).is_empty());
    }

    #[test]
    fn heading_count_mismatch_is_an_error() {
        let orig = "# One\n\n## Two\n\nbody";
        let comp = "# One\n\nbody";
        let errors = validate(orig, comp);
        assert!(errors.iter().any(|e| e.contains("Heading count mismatch")), "{errors:?}");
    }

    #[test]
    fn code_block_change_is_an_error() {
        let orig = "```py\nprint(1)\n```\n";
        let comp = "```py\nprint(2)\n```\n";
        let errors = validate(orig, comp);
        assert!(errors.iter().any(|e| e.contains("Code blocks not preserved")), "{errors:?}");
    }

    #[test]
    fn lost_url_is_an_error() {
        let orig = "see https://x.com for details";
        let comp = "see the docs for details";
        let errors = validate(orig, comp);
        assert!(errors.iter().any(|e| e.contains("URL mismatch")), "{errors:?}");
    }

    #[test]
    fn lost_inline_code_is_an_error() {
        let orig = "run `cargo test` to check";
        let comp = "run the tests to check";
        let errors = validate(orig, comp);
        assert!(errors.iter().any(|e| e.contains("Inline code lost")), "{errors:?}");
    }

    #[test]
    fn nested_fences_extracted_as_one_block() {
        let text = "````md\nouter\n```py\nprint(1)\n```\nouter\n````\n";
        let blocks = extract_code_blocks(text);
        assert_eq!(blocks.len(), 1, "{blocks:?}");
        assert!(blocks[0].contains("print(1)"));
    }
}
