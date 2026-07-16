//! Split/rejoin YAML frontmatter. CRLF-safe (`\r?\n` throughout) — the
//! Python version's equivalent regex only matched `\n` and silently missed
//! frontmatter on CRLF-saved (Windows-edited) files.

use regex::Regex;
use std::sync::LazyLock;

static FRONTMATTER_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)\A(---\r?\n.*?\r?\n---\r?\n)(.*)\z").unwrap());

/// Split YAML frontmatter from body. Returns (frontmatter, body). Files
/// without frontmatter pass through unchanged (frontmatter = "").
pub fn split(text: &str) -> (String, String) {
    match FRONTMATTER_REGEX.captures(text) {
        Some(caps) => (caps[1].to_string(), caps[2].to_string()),
        None => (String::new(), text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lf_frontmatter() {
        let text = "---\nname: x\n---\nbody here";
        let (fm, body) = split(text);
        assert_eq!(fm, "---\nname: x\n---\n");
        assert_eq!(body, "body here");
    }

    #[test]
    fn splits_crlf_frontmatter() {
        let text = "---\r\nname: x\r\n---\r\nbody here";
        let (fm, body) = split(text);
        assert_eq!(fm, "---\r\nname: x\r\n---\r\n");
        assert_eq!(body, "body here");
    }

    #[test]
    fn passes_through_when_no_frontmatter() {
        let text = "just a body, no frontmatter";
        let (fm, body) = split(text);
        assert_eq!(fm, "");
        assert_eq!(body, text);
    }

    #[test]
    fn rejoin_round_trips() {
        let text = "---\nname: x\n---\nbody here";
        let (fm, body) = split(text);
        assert_eq!(format!("{fm}{body}"), text);
    }
}
