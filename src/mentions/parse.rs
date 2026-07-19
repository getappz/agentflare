use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MentionKind {
    Item,
    Asset,
    Search,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    pub kind: MentionKind,
    pub value: String,
}

/// `@I:uuid`, `@A:uuid`, `@search:query` (unquoted, stops at whitespace/`,`/`;`)
/// or `@search:"multi word query"` (quoted, for values containing spaces).
static MENTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)@(I|A|search):(?:"([^"]+)"|([^\s,;]+))"#)
        .expect("static mention regex is valid")
});

/// Cap on mentions resolved per prompt — keeps injected context bounded.
const MAX_MENTIONS: usize = 5;

pub fn parse_mentions(prompt: &str) -> Vec<Mention> {
    MENTION_RE
        .captures_iter(prompt)
        .filter_map(|c| {
            let kind = match c.get(1)?.as_str().to_ascii_lowercase().as_str() {
                "i" => MentionKind::Item,
                "a" => MentionKind::Asset,
                "search" => MentionKind::Search,
                _ => return None,
            };
            // Quoted values are used verbatim; unquoted values trim trailing
            // prose punctuation a bare uuid/query would otherwise absorb
            // (e.g. "see @I:abc123." at the end of a sentence).
            let value = match c.get(2) {
                Some(quoted) => quoted.as_str().to_string(),
                None => c
                    .get(3)?
                    .as_str()
                    .trim_end_matches(['.', ')', ',', ':'])
                    .to_string(),
            };
            if value.is_empty() {
                return None;
            }
            Some(Mention { kind, value })
        })
        .take(MAX_MENTIONS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_item_and_asset_refs() {
        let refs = parse_mentions("see @I:abc-123 and @A:def-456");
        assert_eq!(
            refs,
            vec![
                Mention {
                    kind: MentionKind::Item,
                    value: "abc-123".into()
                },
                Mention {
                    kind: MentionKind::Asset,
                    value: "def-456".into()
                },
            ]
        );
    }

    #[test]
    fn quoted_search_captures_multi_word_query() {
        let refs = parse_mentions(r#"@search:"bug login timeout""#);
        assert_eq!(
            refs,
            vec![Mention {
                kind: MentionKind::Search,
                value: "bug login timeout".into()
            }]
        );
    }

    #[test]
    fn unquoted_search_stops_at_first_space() {
        let refs = parse_mentions("@search:bug login timeout");
        assert_eq!(
            refs,
            vec![Mention {
                kind: MentionKind::Search,
                value: "bug".into()
            }]
        );
    }

    #[test]
    fn trims_trailing_prose_punctuation_from_unquoted_values() {
        let refs = parse_mentions("check @I:abc123.");
        assert_eq!(refs[0].value, "abc123");
    }

    #[test]
    fn is_case_insensitive_on_prefix() {
        let refs = parse_mentions("@i:abc @a:def @SEARCH:xyz");
        assert_eq!(refs.len(), 3);
    }

    #[test]
    fn caps_at_max_mentions() {
        let prompt = (0..10)
            .map(|i| format!("@I:id{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(parse_mentions(&prompt).len(), MAX_MENTIONS);
    }

    #[test]
    fn ignores_prompt_with_no_mentions() {
        assert!(parse_mentions("just a normal prompt").is_empty());
    }
}
