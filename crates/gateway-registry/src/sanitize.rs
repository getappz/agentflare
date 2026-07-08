//! Bounds and cleans up whatever a downstream `tools/list` response hands
//! back before it's indexed and surfaced to the LLM via `gateway_search`.
//! A downstream server (especially a third-party or compromised one) fully
//! controls its own tool names/descriptions — this is the one place that
//! data crosses into our own storage and eventually into LLM context, so
//! it's bounded in size and stripped of control characters before that
//! happens. Same category of guard forgemax's manifest sanitization
//! documents; written fresh here, not ported — see `error.rs`'s note on
//! forgemax's FSL license.

use crate::types::ToolEntry;

const MAX_NAME_LEN: usize = 128;
const MAX_DESCRIPTION_LEN: usize = 1024;

/// Restrict a tool name to `[A-Za-z0-9._-]`, truncated to [`MAX_NAME_LEN`].
/// Falls back to `"unnamed"` if nothing safe survives — an empty server-
/// or tool-name would otherwise break `gateway_execute`'s addressing.
fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(MAX_NAME_LEN)
        .collect();
    if cleaned.is_empty() {
        "unnamed".to_string()
    } else {
        cleaned
    }
}

/// Strip control characters (kept: newline/tab, since descriptions are
/// legitimately multi-line) and truncate to [`MAX_DESCRIPTION_LEN`] chars —
/// bounds how much of a single tool's description a hostile downstream
/// server can push into `gateway_search` results / LLM context.
fn sanitize_description(description: &str) -> String {
    description
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(MAX_DESCRIPTION_LEN)
        .collect()
}

pub fn sanitize_tool_entry(entry: ToolEntry) -> ToolEntry {
    ToolEntry {
        name: sanitize_name(&entry.name),
        description: sanitize_description(&entry.description),
        input_schema: entry.input_schema,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, description: &str) -> ToolEntry {
        ToolEntry { name: name.to_string(), description: description.to_string(), input_schema: serde_json::json!({}) }
    }

    #[test]
    fn strips_special_characters_from_name() {
        let sanitized = sanitize_tool_entry(entry("evil<script>alert(1)</script>", "x"));
        assert_eq!(sanitized.name, "evilscriptalert1script");
    }

    #[test]
    fn empty_name_falls_back_to_unnamed() {
        let sanitized = sanitize_tool_entry(entry("<<<>>>", "x"));
        assert_eq!(sanitized.name, "unnamed");
    }

    #[test]
    fn truncates_long_name() {
        let long_name = "a".repeat(500);
        let sanitized = sanitize_tool_entry(entry(&long_name, "x"));
        assert_eq!(sanitized.name.len(), MAX_NAME_LEN);
    }

    #[test]
    fn truncates_long_description() {
        let long_desc = "x".repeat(5000);
        let sanitized = sanitize_tool_entry(entry("tool", &long_desc));
        assert_eq!(sanitized.description.chars().count(), MAX_DESCRIPTION_LEN);
    }

    #[test]
    fn strips_control_characters_but_keeps_newlines_and_tabs() {
        let desc = "line one\n\tindented\x1b[31mred\x07bell";
        let sanitized = sanitize_tool_entry(entry("tool", desc));
        assert!(sanitized.description.contains('\n'));
        assert!(sanitized.description.contains('\t'));
        assert!(!sanitized.description.contains('\x1b'));
        assert!(!sanitized.description.contains('\x07'));
    }

    #[test]
    fn ordinary_names_and_descriptions_pass_through_unchanged() {
        let sanitized = sanitize_tool_entry(entry("find_symbols", "Find symbols by name or kind."));
        assert_eq!(sanitized.name, "find_symbols");
        assert_eq!(sanitized.description, "Find symbols by name or kind.");
    }
}
