//! Coaching rule storage: where rules live on disk, and the CRUD operations
//! backing the `agentflare coaching` subcommand and the SessionStart hook.

use std::path::PathBuf;

use super::rule::{self, CoachingRule};

pub(super) const MAX_RULES: usize = 8;

fn rules_dir() -> PathBuf {
    crate::state::state_dir().join("rules")
}

/// List all coaching rules from the rules directory, sorted by id. Returns
/// an empty vec if the directory doesn't exist or can't be read.
pub(super) fn list_rules() -> Vec<CoachingRule> {
    let dir = rules_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut rules: Vec<CoachingRule> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|n| n.starts_with("coaching-") && n.ends_with(".md"))
                .unwrap_or(false)
        })
        .filter_map(|e| rule::parse_rule_file(&e.path()))
        .collect();

    rules.sort_by(|a, b| a.id.cmp(&b.id));
    rules
}

/// Create or overwrite a coaching rule file. Enforces `MAX_RULES` only when
/// `id` is new — updating an existing rule's content never counts against
/// the cap.
pub fn apply_rule(
    id: &str,
    title: &str,
    body: &str,
    trigger: Option<rule::RuleTrigger>,
) -> Result<CoachingRule, String> {
    if !rule::is_valid_rule_id(id) {
        return Err(format!(
            "invalid rule id '{id}': must be 1-10 chars, start with a letter, and contain only letters, digits, or hyphens"
        ));
    }

    let existing = list_rules();
    let is_overwrite = existing.iter().any(|r| r.id == id);
    if !is_overwrite && existing.len() >= MAX_RULES {
        return Err(format!(
            "maximum {MAX_RULES} coaching rules reached — remove one first"
        ));
    }

    rule::write_rule_file(&rules_dir(), id, title, body, trigger.as_ref())
        .map_err(|e| format!("failed to write rule file: {e}"))?;

    list_rules()
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| "rule written but could not be re-read".to_string())
}

/// Remove a coaching rule file by id.
pub(super) fn remove_rule(id: &str) -> Result<(), String> {
    if !rule::is_valid_rule_id(id) {
        return Err(format!("invalid rule id '{id}'"));
    }
    let path = rules_dir().join(format!("coaching-{id}.md"));
    if !path.exists() {
        return Err(format!("rule not found: {id}"));
    }
    std::fs::remove_file(&path).map_err(|e| format!("failed to remove rule file: {e}"))
}

/// Bodies of rules with no `# Trigger:` declared, in id order — what
/// `hook session-start` surfaces unconditionally.
pub fn untriggered_rule_bodies() -> Vec<String> {
    list_rules()
        .into_iter()
        .filter(|r| r.trigger.is_none())
        .map(|r| r.body)
        .collect()
}

/// Bodies of rules whose trigger declares this exact tool name.
pub fn rule_bodies_for_tool(tool_name: &str) -> Vec<String> {
    list_rules()
        .into_iter()
        .filter(|r| {
            r.trigger
                .as_ref()
                .is_some_and(|trigger| trigger.tools.iter().any(|t| t == tool_name))
        })
        .map(|r| r.body)
        .collect()
}

/// Bodies of rules with `auto_match: true` whose title+body BM25-matches
/// `prompt`, via the same ephemeral FTS5 scorer `crate::compact` already
/// built for the PreCompact hook. No numeric threshold: appearing in
/// `score_lines`'s result set is itself the fire/no-fire decision.
pub fn rule_bodies_for_prompt(prompt: &str) -> Vec<String> {
    let candidates: Vec<CoachingRule> = list_rules()
        .into_iter()
        .filter(|r| r.trigger.as_ref().is_some_and(|trigger| trigger.auto_match))
        .collect();
    if candidates.is_empty() {
        return vec![];
    }

    let entries: Vec<crate::compact::LineEntry> = candidates
        .iter()
        .enumerate()
        .map(|(i, r)| crate::compact::LineEntry {
            index: i,
            text: format!("{} {}", r.title, r.body),
        })
        .collect();

    match crate::compact::score_lines(&entries, prompt) {
        Ok(scored) => scored
            .into_iter()
            .map(|s| candidates[s.index].body.clone())
            .collect(),
        Err(_) => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn list_rules_returns_empty_when_directory_missing() {
        with_temp_home(|| {
            assert!(list_rules().is_empty());
        });
    }

    #[test]
    fn apply_rule_then_list_rules_roundtrips() {
        with_temp_home(|| {
            let applied = apply_rule(
                "hygiene",
                "Close sessions promptly",
                "Wrap up each phase before starting the next.",
                None,
            )
            .unwrap();
            assert_eq!(applied.id, "hygiene");
            assert_eq!(applied.title, "Close sessions promptly");
            assert_eq!(applied.body, "Wrap up each phase before starting the next.");

            let rules = list_rules();
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0].id, "hygiene");
        });
    }

    #[test]
    fn apply_rule_rejects_invalid_id() {
        with_temp_home(|| {
            let err = apply_rule("1bad", "Title", "Body", None).unwrap_err();
            assert!(err.contains("invalid rule id"));
            assert!(list_rules().is_empty());
        });
    }

    #[test]
    fn apply_rule_enforces_max_rules_for_new_ids() {
        with_temp_home(|| {
            for i in 0..MAX_RULES {
                apply_rule(&format!("r{i}"), "T", "B", None).unwrap();
            }
            let err = apply_rule("one-more", "T", "B", None).unwrap_err();
            assert!(err.contains("maximum"));
            assert_eq!(list_rules().len(), MAX_RULES);
        });
    }

    #[test]
    fn apply_rule_allows_overwriting_existing_id_at_capacity() {
        with_temp_home(|| {
            for i in 0..MAX_RULES {
                apply_rule(&format!("r{i}"), "T", "B", None).unwrap();
            }
            let updated = apply_rule("r0", "New Title", "New Body", None).unwrap();
            assert_eq!(updated.title, "New Title");
            assert_eq!(list_rules().len(), MAX_RULES);
        });
    }

    #[test]
    fn remove_rule_deletes_existing_file() {
        with_temp_home(|| {
            apply_rule("hygiene", "T", "B", None).unwrap();
            remove_rule("hygiene").unwrap();
            assert!(list_rules().is_empty());
        });
    }

    #[test]
    fn remove_rule_errors_when_not_found() {
        with_temp_home(|| {
            let err = remove_rule("missing").unwrap_err();
            assert!(err.contains("not found"));
        });
    }

    #[test]
    fn list_rules_skips_malformed_files_without_crashing() {
        with_temp_home(|| {
            std::fs::create_dir_all(rules_dir()).unwrap();
            std::fs::write(rules_dir().join("coaching-broken.md"), "").unwrap();
            apply_rule("good", "T", "B", None).unwrap();

            let rules = list_rules();
            assert_eq!(
                rules.len(),
                1,
                "malformed file with no # Applied: header is skipped"
            );
            assert_eq!(rules[0].id, "good");
        });
    }

    #[test]
    fn untriggered_rule_bodies_returns_all_untriggered_bodies_in_id_order() {
        with_temp_home(|| {
            apply_rule("b-rule", "Title B", "Body B", None).unwrap();
            apply_rule("a-rule", "Title A", "Body A", None).unwrap();
            assert_eq!(
                untriggered_rule_bodies(),
                vec!["Body A".to_string(), "Body B".to_string()]
            );
        });
    }

    #[test]
    fn apply_rule_body_with_dashes_line_is_not_truncated() {
        with_temp_home(|| {
            let applied =
                apply_rule("dashes", "Title", "before\n---\nafter", None).unwrap();
            assert!(
                applied.body.contains("before"),
                "body should contain text before the --- line: {}",
                applied.body
            );
            assert!(
                applied.body.contains("after"),
                "body should contain text after the --- line: {}",
                applied.body
            );

            let rules = list_rules();
            assert_eq!(rules.len(), 1);
            assert!(rules[0].body.contains("before"));
            assert!(rules[0].body.contains("after"));
        });
    }

    #[test]
    fn apply_rule_title_with_em_dash_is_not_truncated() {
        with_temp_home(|| {
            let applied =
                apply_rule("emdash", "Foo \u{2014} Bar", "Body", None).unwrap();
            assert_eq!(applied.title, "Foo \u{2014} Bar");

            let rules = list_rules();
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0].title, "Foo \u{2014} Bar");
        });
    }

    #[test]
    fn apply_rule_with_trigger_roundtrips() {
        with_temp_home(|| {
            let trigger = rule::RuleTrigger {
                tools: vec!["mcp__flare__review".to_string()],
                auto_match: true,
            };
            let applied = apply_rule("revfix", "Title", "Body", Some(trigger.clone())).unwrap();
            assert_eq!(applied.trigger, Some(trigger.clone()));

            let rules = list_rules();
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0].trigger, Some(trigger));
        });
    }

    #[test]
    fn apply_rule_without_trigger_is_untriggered() {
        with_temp_home(|| {
            let applied = apply_rule("hygiene", "Title", "Body", None).unwrap();
            assert_eq!(applied.trigger, None);
        });
    }

    #[test]
    fn untriggered_rule_bodies_excludes_triggered_rules() {
        with_temp_home(|| {
            apply_rule("plain", "T", "Plain body", None).unwrap();
            apply_rule(
                "scoped",
                "T",
                "Scoped body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
            )
            .unwrap();

            assert_eq!(untriggered_rule_bodies(), vec!["Plain body".to_string()]);
        });
    }

    #[test]
    fn rule_bodies_for_tool_matches_exact_tool_name_only() {
        with_temp_home(|| {
            apply_rule(
                "scoped",
                "T",
                "Scoped body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
            )
            .unwrap();

            assert_eq!(
                rule_bodies_for_tool("mcp__flare__review"),
                vec!["Scoped body".to_string()]
            );
            assert!(rule_bodies_for_tool("mcp__flare__comment").is_empty());
            assert!(rule_bodies_for_tool("mcp__flare__revie").is_empty());
        });
    }

    #[test]
    fn rule_bodies_for_prompt_fires_auto_match_rule_on_shared_term() {
        with_temp_home(|| {
            apply_rule(
                "revfix",
                "Reviews ship with fixes",
                "Every review finding needs a diff.",
                Some(rule::RuleTrigger {
                    tools: vec![],
                    auto_match: true,
                }),
            )
            .unwrap();

            let bodies = rule_bodies_for_prompt("please review this change");
            assert_eq!(bodies, vec!["Every review finding needs a diff.".to_string()]);
        });
    }

    #[test]
    fn rule_bodies_for_prompt_does_not_fire_on_unrelated_prompt() {
        with_temp_home(|| {
            apply_rule(
                "revfix",
                "Reviews ship with fixes",
                "Every review finding needs a diff.",
                Some(rule::RuleTrigger {
                    tools: vec![],
                    auto_match: true,
                }),
            )
            .unwrap();

            assert!(rule_bodies_for_prompt("what's the weather like today").is_empty());
        });
    }

    #[test]
    fn rule_bodies_for_prompt_ignores_non_auto_match_rules() {
        with_temp_home(|| {
            apply_rule("plain", "T", "Plain body", None).unwrap();
            apply_rule(
                "tool-only",
                "T",
                "Tool only body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
            )
            .unwrap();

            assert!(rule_bodies_for_prompt("anything at all").is_empty());
        });
    }

    #[test]
    fn rule_bodies_for_prompt_returns_empty_when_no_auto_match_candidates() {
        with_temp_home(|| {
            assert!(rule_bodies_for_prompt("anything").is_empty());
        });
    }
}
