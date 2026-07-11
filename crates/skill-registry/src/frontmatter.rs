//! YAML frontmatter parsing for SKILL.md files.

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
pub struct Frontmatter {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Parse `---\n<yaml>\n---\n<body>`. Returns None when the file has no
/// well-formed frontmatter block or the YAML does not parse.
pub fn parse_frontmatter(text: &str) -> Option<(Frontmatter, &str)> {
    let rest = text.strip_prefix("---")?;
    let rest = rest
        .strip_prefix("\r\n")
        .or_else(|| rest.strip_prefix('\n'))?;
    // Closing fence: a line that is exactly `---` (allow trailing \r).
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" && !line.starts_with("----") {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            let fm: Frontmatter = serde_yaml::from_str(yaml).ok()?;
            return Some((fm, body));
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_frontmatter_and_body() {
        let (fm, body) =
            parse_frontmatter("---\nname: live\ndescription: Use when X\n---\nBody here\n")
                .unwrap();
        assert_eq!(fm.name.as_deref(), Some("live"));
        assert_eq!(fm.description.as_deref(), Some("Use when X"));
        assert_eq!(body.trim(), "Body here");
    }

    #[test]
    fn parses_folded_multiline_description() {
        let text = "---\nname: x\ndescription: >\n  first part\n  second part\n---\nb";
        let (fm, _) = parse_frontmatter(text).unwrap();
        assert!(fm.description.unwrap().contains("second part"));
    }

    #[test]
    fn tolerates_unknown_fields() {
        let text = "---\nname: x\ndescription: d\nallowed-tools:\n  - a\n  - b\n---\nb";
        let (fm, _) = parse_frontmatter(text).unwrap();
        assert_eq!(fm.name.as_deref(), Some("x"));
    }

    #[test]
    fn returns_none_without_frontmatter() {
        assert!(parse_frontmatter("# Just markdown\n").is_none());
    }

    #[test]
    fn returns_none_on_unterminated_block() {
        assert!(parse_frontmatter("---\nname: x\nno closing fence").is_none());
    }
}
