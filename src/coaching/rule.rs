//! Coaching rule data model: the `CoachingRule` struct and the
//! `coaching-<id>.md` file format (parsing + serialization).

/// Whether a rule ships as an agentflare default (drift-protected across
/// version bumps) or is a user override (always wins, always overwrites).
#[derive(Debug, Clone, PartialEq, clap::ValueEnum)]
pub enum RuleTier {
    Builtin,
    Override,
}

impl RuleTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleTier::Builtin => "builtin",
            RuleTier::Override => "override",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "builtin" => Some(RuleTier::Builtin),
            "override" => Some(RuleTier::Override),
            _ => None,
        }
    }
}

/// A coaching rule loaded from a `coaching-<id>.md` file.
#[derive(Debug)]
pub struct CoachingRule {
    pub id: String,
    pub title: String,
    pub body: String,
    pub applied_at: String,
    pub trigger: Option<RuleTrigger>,
    pub tier: RuleTier,
    pub sync: Vec<String>,
    pub enforced: bool,
    pub cooldown_secs: Option<u64>,
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

const KNOWN_SYNC_HOSTS: &[&str] = &[
    "claude-code",
    "opencode",
    "cursor",
    "codex",
    "windsurf",
    "vscode-copilot",
    "cline",
];

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
    sync: &[String],
) -> Result<(), String> {
    if title.contains('\n') {
        return Err("rule title must not contain newlines".to_string());
    }
    for host in sync {
        if !KNOWN_SYNC_HOSTS.contains(&host.as_str()) {
            return Err(format!(
                "invalid sync host '{host}': must be one of {}",
                KNOWN_SYNC_HOSTS.join(", ")
            ));
        }
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
    let mut tier = RuleTier::Override;
    let mut sync = Vec::new();
    let mut enforced = false;
    let mut cooldown_secs = None;
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
            } else if let Some(rest) = line.strip_prefix("# Tier:") {
                if let Some(t) = RuleTier::parse(rest) {
                    tier = t;
                }
            } else if let Some(rest) = line.strip_prefix("# Sync:") {
                sync = rest
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            } else if let Some(rest) = line.strip_prefix("# Enforce:") {
                enforced = rest.trim().eq_ignore_ascii_case("true");
            } else if let Some(rest) = line.strip_prefix("# Cooldown: ") {
                cooldown_secs = rest.trim().parse().ok();
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
        tier,
        sync,
        enforced,
        cooldown_secs,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write_rule_file(
    dir: &std::path::Path,
    id: &str,
    title: &str,
    body: &str,
    trigger: Option<&RuleTrigger>,
    tier: RuleTier,
    sync: &[String],
    enforced: bool,
    cooldown_secs: Option<u64>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let date = chrono::Local::now().date_naive();
    let trigger_line = match trigger {
        Some(t) => format!("\n# Trigger: {}", format_trigger_line(t)),
        None => String::new(),
    };
    let sync_line = if sync.is_empty() {
        String::new()
    } else {
        format!("\n# Sync: {}", sync.join(", "))
    };
    let enforce_line = if enforced {
        "\n# Enforce: true".to_string()
    } else {
        String::new()
    };
    let cooldown_line = match cooldown_secs {
        Some(v) => format!("\n# Cooldown: {v}"),
        None => String::new(),
    };
    let content = format!(
        "---\n# Pattern: {id} \u{2014} {title}\n# Applied: {date}{trigger_line}\n# Tier: {}{sync_line}{enforce_line}{cooldown_line}\n---\n\n{body}\n",
        tier.as_str()
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
        assert!(validate_rule_fields("bad\ntitle", None, &[]).is_err());
        assert!(validate_rule_fields("fine title", None, &[]).is_ok());
    }

    #[test]
    fn validate_rule_fields_rejects_empty_trigger() {
        let empty = RuleTrigger {
            tools: vec![],
            auto_match: false,
        };
        assert!(validate_rule_fields("Title", Some(&empty), &[]).is_err());
    }

    #[test]
    fn validate_rule_fields_rejects_delimiter_in_tool_name() {
        let bad = RuleTrigger {
            tools: vec!["a,b".to_string()],
            auto_match: false,
        };
        assert!(validate_rule_fields("Title", Some(&bad), &[]).is_err());

        let ok = RuleTrigger {
            tools: vec!["mcp__flare__review".to_string()],
            auto_match: false,
        };
        assert!(validate_rule_fields("Title", Some(&ok), &[]).is_ok());
    }

    #[test]
    fn validate_rule_fields_rejects_unknown_sync_host() {
        assert!(validate_rule_fields("Title", None, &["unknown".to_string()]).is_err());
        assert!(validate_rule_fields("Title", None, &["claude-code".to_string()]).is_ok());
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
        // `COUNTER` alone isn't enough: nextest runs each test in its own
        // process, so every process's counter starts back at 0 and every
        // test calling this ends up on the *same* path -- a race that
        // Windows' stricter file locking turns into a hard failure instead
        // of the silent tolerance Unix gives it. Mix in the pid so
        // concurrent test processes never collide; the counter still
        // covers multiple calls within one process (old-style `cargo test`,
        // or more than one call in the same test).
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("agentflare-coaching-rule-test-{pid}-{n}"));
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
            RuleTier::Override,
            &[],
            false,
            None,
        )
        .unwrap();

        let rule = parse_rule_file(&dir.join("coaching-revfix.md")).unwrap();
        assert_eq!(rule.trigger, Some(trigger));
        assert_eq!(rule.tier, RuleTier::Override);
        assert!(rule.sync.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_then_parse_roundtrips_no_trigger() {
        let dir = temp_dir_for_test();
        write_rule_file(
            &dir,
            "hygiene",
            "Title",
            "Body",
            None,
            RuleTier::Override,
            &[],
            false,
            None,
        )
        .unwrap();

        let rule = parse_rule_file(&dir.join("coaching-hygiene.md")).unwrap();
        assert_eq!(rule.trigger, None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn write_then_parse_roundtrips_tier_and_sync() {
        let dir = temp_dir_for_test();
        write_rule_file(
            &dir,
            "search17",
            "Search",
            "Body",
            None,
            RuleTier::Builtin,
            &["claude-code".to_string(), "opencode".to_string()],
            false,
            None,
        )
        .unwrap();

        let rule = parse_rule_file(&dir.join("coaching-search17.md")).unwrap();
        assert_eq!(rule.tier, RuleTier::Builtin);
        assert_eq!(
            rule.sync,
            vec!["claude-code".to_string(), "opencode".to_string()]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_old_file_without_tier_sync_defaults_to_override() {
        let dir = temp_dir_for_test();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("coaching-old.md"),
            "---\n# Pattern: old \u{2014} Old\n# Applied: 2026-01-01\n---\n\nBody\n",
        )
        .unwrap();
        let rule = parse_rule_file(&dir.join("coaching-old.md")).unwrap();
        assert_eq!(rule.tier, RuleTier::Override);
        assert!(rule.sync.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_rule_file_skips_file_with_invalid_id_in_filename() {
        let dir = temp_dir_for_test();
        write_rule_file(
            &dir,
            "hygiene",
            "Title",
            "Body",
            None,
            RuleTier::Override,
            &[],
            false,
            None,
        )
        .unwrap();
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

    #[test]
    fn write_then_parse_roundtrips_enforced_flag() {
        let dir = temp_dir_for_test();
        write_rule_file(
            &dir,
            "search17",
            "Search",
            "Body",
            None,
            RuleTier::Builtin,
            &[],
            true,
            None,
        )
        .unwrap();

        let rule = parse_rule_file(&dir.join("coaching-search17.md")).unwrap();
        assert!(rule.enforced);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_old_file_without_enforce_line_defaults_to_false() {
        let dir = temp_dir_for_test();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("coaching-old.md"),
            "---\n# Pattern: old \u{2014} Old\n# Applied: 2026-01-01\n---\n\nBody\n",
        )
        .unwrap();
        let rule = parse_rule_file(&dir.join("coaching-old.md")).unwrap();
        assert!(!rule.enforced);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cooldown_secs_round_trips_through_write_and_parse() {
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            crate::coaching::apply_rule_with_cooldown(
                "hygiene",
                "Title",
                "Body",
                None,
                RuleTier::Override,
                vec![],
                Some(600),
            )
            .unwrap();
            let rules = crate::coaching::list_rules();
            assert_eq!(rules[0].cooldown_secs, Some(600));
        });
    }

    #[test]
    fn cooldown_secs_defaults_to_none_when_absent() {
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            crate::coaching::apply_rule(
                "hygiene",
                "Title",
                "Body",
                None,
                RuleTier::Override,
                vec![],
            )
            .unwrap();
            let rules = crate::coaching::list_rules();
            assert_eq!(rules[0].cooldown_secs, None);
        });
    }
}
