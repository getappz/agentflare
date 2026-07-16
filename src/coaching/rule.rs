//! Coaching rule data model: the `CoachingRule` struct and the
//! `coaching-<id>.md` file format (parsing + serialization).

/// A coaching rule loaded from a `coaching-<id>.md` file.
#[derive(Debug)]
pub struct CoachingRule {
    pub id: String,
    pub title: String,
    pub body: String,
    pub applied_at: String,
    pub trigger: Option<RuleTrigger>,
}

/// Declares when a rule should fire contextually instead of at every
/// SessionStart. A rule fires if its tool trigger OR its auto-relevance
/// trigger matches (OR across kinds).
#[derive(Debug, Clone, PartialEq)]
pub struct RuleTrigger {
    pub tools: Vec<String>,
    /// When true, this rule's title+body is scored via BM25 against the
    /// current prompt (see store::rule_bodies_for_prompt) instead of
    /// requiring a hand-authored keyword list.
    pub auto_match: bool,
}

/// Validate a rule id: non-empty, max 10 chars, starts with an ASCII
/// letter, remaining chars ASCII alphanumeric or `-`. Ported from
/// claude-view's is_valid_pattern_id.
pub(super) fn is_valid_rule_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 10
        && id.starts_with(|c: char| c.is_ascii_alphabetic())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Validates fields that will be serialized into a rule file's header.
/// A title containing a newline would break the one-line-per-header-field
/// format; commas/semicolons or newlines in a tool name would corrupt the
/// tool:a,b; auto trigger grammar. A Some(RuleTrigger) with no tools and
/// auto_match: false is rejected too, it round-trips back to None on
/// reparse, so writing it would silently discard the caller's intent
/// instead of persisting it.
pub(super) fn validate_rule_fields(
    title: &str,
    trigger: Option<&RuleTrigger>,
) -> Result<(), String> {
    if title.contains('\n') {
        return Err("rule title must not contain newlines".to_string());
    }
    let Some(trigger) = trigger else {
        return Ok(());
    };
    if trigger.tools.is_empty() && !trigger.auto_match {
        return Err(
            "trigger has no tools and auto_match=false, pass None instead of an empty trigger"
                .to_string(),
        );
    }
    for tool in &trigger.tools {
        if tool.is_empty() || tool.contains(['\n', ',', ';']) {
            return Err(format!(
                "invalid tool name in trigger {tool:?}: must be non-empty and must not contain newlines, commas, or semicolons"
            ));
        }
    }
    Ok(())
}

/// Parse a Trigger line body (the text after "# Trigger:"). Segments are
/// semicolon-separated; a segment of exactly auto (case-insensitive)
/// enables BM25 auto-relevance matching, a tool:<csv> segment declares
/// exact tool names. Unknown segment kinds are ignored rather than
/// invalidating the whole line. Returns None if nothing recognizable was
/// found, callers should treat that as malformed and fall back to
/// untriggered rather than erroring.
fn parse_trigger_line(rest: &str) -> Option<RuleTrigger> {
    let mut tools = Vec::new();
    let mut auto_match = false;
    for segment in rest.split(';') {
        let segment = segment.trim();
        if segment.is_empty() {
            continue;
        }
        if segment.eq_ignore_ascii_case("auto") {
            auto_match = true;
            continue;
        }
        let Some((kind, list)) = segment.split_once(':') else {
            continue;
        };
        if kind.trim().eq_ignore_ascii_case("tool") {
            tools.extend(
                list.split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(String::from),
            );
        }
    }
    if tools.is_empty() && !auto_match {
        None
    } else {
        Some(RuleTrigger { tools, auto_match })
    }
}

/// Inverse of parse_trigger_line, renders a RuleTrigger back into the
/// tool:a,b; auto text that goes after "# Trigger:".
fn format_trigger_line(trigger: &RuleTrigger) -> String {
    let mut parts = Vec::new();
    if !trigger.tools.is_empty() {
        parts.push(format!("tool:{}", trigger.tools.join(",")));
    }
    if trigger.auto_match {
        parts.push("auto".to_string());
    }
    parts.join("; ")
}

/// Parse a single coaching-<id>.md file into a CoachingRule. Returns None
/// if the filename doesn't match the expected pattern, its id isn't a
/// valid rule id, or if a matching file has no valid "# Applied:" header,
/// such files are skipped entirely (not included with blank fields) so one
/// bad file can never take down the whole listing.
pub(super) fn parse_rule_file(path: &std::path::Path) -> Option<CoachingRule> {
    let content = std::fs::read_to_string(path).ok()?;
    let filename = path.file_stem()?.to_str()?;
    let id = filename.strip_prefix("coaching-")?.to_string();
    if !is_valid_rule_id(&id) {
        return None;
    }

    let mut title = String::new();
    let mut applied_at = String::new();
    let mut trigger = None;
    let mut in_header = false;
    let mut header_done = false;
    let mut body_lines = Vec::new();

    for line in content.lines() {
        if line.starts_with("---") && !header_done {
            in_header = !in_header;
            if !in_header {
                header_done = true;
            }
            continue;
        }
        if in_header {
            if let Some(rest) = line.strip_prefix("# Pattern:") {
                if let Some(t) = rest.split_once('\u{2014}').map(|x| x.1) {
                    title = t.trim().to_string();
                }
            } else if let Some(rest) = line.strip_prefix("# Applied:") {
                applied_at = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("# Trigger:") {
                trigger = parse_trigger_line(rest);
                if trigger.is_none() {
                    eprintln!(
                        "[agentflare] coaching: malformed or empty Trigger line, treating as untriggered: {rest:?}"
                    );
                }
            }
        } else if !line.is_empty() {
            body_lines.push(line);
        }
    }

    if applied_at.is_empty() {
        return None;
    }

    Some(CoachingRule {
        id,
        title,
        body: body_lines.join(" "),
        applied_at,
        trigger,
    })
}

pub(super) fn write_rule_file(
    dir: &std::path::Path,
    id: &str,
    title: &str,
    body: &str,
    trigger: Option<&RuleTrigger>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let date = chrono::Local::now().date_naive();
    let trigger_line = match trigger {
        Some(t) => format!("\n# Trigger: {}", format_trigger_line(t)),
        None => String::new(),
    };
    let content = format!(
        "---\n# Pattern: {id} \u{2014} {title}\n# Applied: {date}{trigger_line}\n---\n\n{body}\n"
    );
    let final_path = dir.join(format!("coaching-{id}.md"));
    let tmp_path = dir.join(format!("coaching-{id}.md.tmp"));
    std::fs::write(&tmp_path, content)?;
    std::fs::rename(&tmp_path, &final_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_rule_id_accepts_short_alpha_start_ids() {
        assert!(is_valid_rule_id("hygiene"));
        assert!(is_valid_rule_id("a"));
        assert!(is_valid_rule_id("model-1"));
    }

    #[test]
    fn is_valid_rule_id_rejects_empty_too_long_numeric_start_or_bad_chars() {
        assert!(!is_valid_rule_id(""));
        assert!(!is_valid_rule_id("waytoolongforanid"));
        assert!(!is_valid_rule_id("1abc"));
        assert!(!is_valid_rule_id("has space"));
        assert!(!is_valid_rule_id("has_underscore"));
    }

    #[test]
    fn validate_rule_fields_rejects_newline_in_title() {
        assert!(validate_rule_fields("bad\ntitle", None).is_err());
        assert!(validate_rule_fields("fine title", None).is_ok());
    }

    #[test]
    fn validate_rule_fields_rejects_empty_trigger() {
        let empty = RuleTrigger {
            tools: vec![],
            auto_match: false,
        };
        assert!(validate_rule_fields("Title", Some(&empty)).is_err());
    }

    #[test]
    fn validate_rule_fields_rejects_delimiter_in_tool_name() {
        let bad = RuleTrigger {
            tools: vec!["a,b".to_string()],
            auto_match: false,
        };
        assert!(validate_rule_fields("Title", Some(&bad)).is_err());

        let ok = RuleTrigger {
            tools: vec!["mcp__flare__review".to_string()],
            auto_match: false,
        };
        assert!(validate_rule_fields("Title", Some(&ok)).is_ok());
    }

    #[test]
    fn parse_trigger_line_reads_tools_only() {
        assert_eq!(
            parse_trigger_line("tool:mcp__flare__review,mcp__flare__comment"),
            Some(RuleTrigger {
                tools: vec![
                    "mcp__flare__review".to_string(),
                    "mcp__flare__comment".to_string()
                ],
                auto_match: false,
            })
        );
    }

    #[test]
    fn parse_trigger_line_reads_bare_auto() {
        assert_eq!(
            parse_trigger_line("auto"),
            Some(RuleTrigger {
                tools: vec![],
                auto_match: true,
            })
        );
    }

    #[test]
    fn parse_trigger_line_reads_auto_case_insensitive() {
        assert_eq!(
            parse_trigger_line("AUTO"),
            Some(RuleTrigger {
                tools: vec![],
                auto_match: true,
            })
        );
    }

    #[test]
    fn parse_trigger_line_reads_both_kinds() {
        assert_eq!(
            parse_trigger_line("tool:mcp__flare__review; auto"),
            Some(RuleTrigger {
                tools: vec!["mcp__flare__review".to_string()],
                auto_match: true,
            })
        );
    }

    #[test]
    fn parse_trigger_line_ignores_unknown_kind_but_keeps_known() {
        assert_eq!(
            parse_trigger_line("bogus:x; tool:mcp__flare__review"),
            Some(RuleTrigger {
                tools: vec!["mcp__flare__review".to_string()],
                auto_match: false,
            })
        );
    }

    #[test]
    fn parse_trigger_line_returns_none_for_empty_or_malformed() {
        assert_eq!(parse_trigger_line(""), None);
        assert_eq!(parse_trigger_line("   "), None);
        assert_eq!(parse_trigger_line("bogus with no colon"), None);
    }

    fn temp_dir_for_test() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("agentflare-coaching-rule-test-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn write_then_parse_roundtrips_trigger() {
        let dir = temp_dir_for_test();
        let trigger = RuleTrigger {
            tools: vec!["mcp__flare__review".to_string()],
            auto_match: true,
        };
        write_rule_file(
            &dir,
            "revfix",
            "Reviews ship with fixes",
            "Body text",
            Some(&trigger),
        )
        .unwrap();

        let rule = parse_rule_file(&dir.join("coaching-revfix.md")).unwrap();
        assert_eq!(rule.trigger, Some(trigger));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_then_parse_roundtrips_no_trigger() {
        let dir = temp_dir_for_test();
        write_rule_file(&dir, "hygiene", "Title", "Body", None).unwrap();

        let rule = parse_rule_file(&dir.join("coaching-hygiene.md")).unwrap();
        assert_eq!(rule.trigger, None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_rule_file_skips_file_with_invalid_id_in_filename() {
        let dir = temp_dir_for_test();
        write_rule_file(&dir, "hygiene", "Title", "Body", None).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("coaching-not a valid id.md"),
            "---\n# Pattern: x \u{2014} y\n# Applied: 2026-01-01\n---\n\nBody\n",
        )
        .unwrap();

        assert!(parse_rule_file(&dir.join("coaching-not a valid id.md")).is_none());
        assert!(parse_rule_file(&dir.join("coaching-hygiene.md")).is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
