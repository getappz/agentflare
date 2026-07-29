//! Coaching rule storage: where rules live on disk, and the CRUD operations
//! backing the `agentflare coaching` subcommand and the SessionStart hook.

use std::path::PathBuf;

use serde_json::Value;

use super::rule::{self, CoachingRule, RuleTier};

pub(super) const MAX_RULES: usize = 8;

fn rules_dir() -> PathBuf {
    crate::state::state_dir().join("rules")
}

/// A simple cross-process advisory lock over the rules directory: create_new
/// on a sentinel file fails if another process already holds it, so at most
/// one process can be inside apply_rule/remove_rule's read-check-write
/// sequence at a time. Removed on drop. A holder that crashed without
/// cleaning up must never wedge the directory shut, so a lock held for more
/// than about two seconds is treated as stale and broken rather than waited
/// on forever.
struct RulesLock {
    path: PathBuf,
}

impl RulesLock {
    fn acquire(dir: &std::path::Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(".rules.lock");
        for _ in 0..200 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        }
        let _ = std::fs::remove_file(&path);
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self { path })
    }
}

impl Drop for RulesLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// List all coaching rules from the rules directory, sorted by id. Returns
/// an empty vec if the directory doesn't exist or can't be read.
pub(crate) fn list_rules() -> Vec<CoachingRule> {
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

/// Create or overwrite a coaching rule file. Enforces MAX_RULES only when
/// id is new, updating an existing rule's content never counts against
/// the cap. Serialized across processes by RulesLock so two concurrent
/// callers can never both observe room under MAX_RULES and both write.
pub fn apply_rule(
    id: &str,
    title: &str,
    body: &str,
    trigger: Option<rule::RuleTrigger>,
    tier: RuleTier,
    sync: Vec<String>,
) -> Result<CoachingRule, String> {
    apply_rule_inner(id, title, body, trigger, tier, sync, None)
}

/// Same as `apply_rule`, but lets the caller set a per-rule pacing cooldown
/// (seconds) instead of inheriting `nudge_pace::DEFAULT_COOLDOWN`.
// flare-code: no CLI/MCP surface calls this yet (only this module's own
// tests do) — upgrade path is a `--cooldown` flag on `agentflare coaching
// apply` once a rule author needs a non-default pace.
#[allow(dead_code)]
pub fn apply_rule_with_cooldown(
    id: &str,
    title: &str,
    body: &str,
    trigger: Option<rule::RuleTrigger>,
    tier: RuleTier,
    sync: Vec<String>,
    cooldown_secs: Option<u64>,
) -> Result<CoachingRule, String> {
    apply_rule_inner(id, title, body, trigger, tier, sync, cooldown_secs)
}

#[allow(clippy::too_many_arguments)]
fn apply_rule_inner(
    id: &str,
    title: &str,
    body: &str,
    trigger: Option<rule::RuleTrigger>,
    tier: RuleTier,
    sync: Vec<String>,
    cooldown_secs: Option<u64>,
) -> Result<CoachingRule, String> {
    if !rule::is_valid_rule_id(id) {
        return Err(format!(
            "invalid rule id '{id}': must be 1-10 chars, start with a letter, and contain only letters, digits, or hyphens"
        ));
    }
    rule::validate_rule_fields(title, trigger.as_ref(), &sync)?;

    let _lock = RulesLock::acquire(&rules_dir())
        .map_err(|e| format!("failed to acquire rules lock: {e}"))?;

    let existing = list_rules();
    let is_overwrite = existing.iter().any(|r| r.id == id);
    let enforced = existing
        .iter()
        .find(|r| r.id == id)
        .map(|r| r.enforced)
        .unwrap_or(false);
    // Preserve the existing rule's cooldown_secs unless the caller explicitly
    // passed one (apply_rule_with_cooldown's Some(n) always wins; a plain
    // apply_rule refresh, which always passes None here, must not silently
    // wipe a cooldown that was set earlier — mirrors how `enforced` is
    // preserved above).
    let cooldown_secs = cooldown_secs.or_else(|| {
        existing
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.cooldown_secs)
    });
    if !is_overwrite && existing.len() >= MAX_RULES {
        return Err(format!(
            "maximum {MAX_RULES} coaching rules reached, remove one first"
        ));
    }

    if is_overwrite
        && tier == rule::RuleTier::Builtin
        && let Some(prev) = existing.iter().find(|r| r.id == id)
        && prev.body != body
    {
        snapshot_previous_body(id, &prev.body)
            .map_err(|e| format!("failed to snapshot previous rule body: {e}"))?;
    }

    rule::write_rule_file(
        &rules_dir(),
        id,
        title,
        body,
        trigger.as_ref(),
        tier.clone(),
        &sync,
        enforced,
        cooldown_secs,
    )
    .map_err(|e| format!("failed to write rule file: {e}"))?;

    list_rules()
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| "rule written but could not be re-read".to_string())
}

/// Flip a rule's `enforced` (MANDATORY) flag without touching its other
/// fields. Only meaningful for rules with a tool trigger — an enforced
/// rule with no tools would never match any PreToolUse call, silently
/// doing nothing, so that combination is rejected rather than allowed to
/// look like it's doing something it isn't.
pub fn set_enforced(id: &str, enforced: bool) -> Result<CoachingRule, String> {
    if !rule::is_valid_rule_id(id) {
        return Err(format!("invalid rule id '{id}'"));
    }
    let _lock = RulesLock::acquire(&rules_dir())
        .map_err(|e| format!("failed to acquire rules lock: {e}"))?;

    let existing = list_rules();
    let target = existing
        .iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("rule not found: {id}"))?;
    if enforced
        && target
            .trigger
            .as_ref()
            .map(|t| t.tools.is_empty())
            .unwrap_or(true)
    {
        return Err(format!(
            "rule '{id}' has no tool trigger \u{2014} enforced rules must declare a tool trigger so the hook knows which calls to block"
        ));
    }

    rule::write_rule_file(
        &rules_dir(),
        id,
        &target.title,
        &target.body,
        target.trigger.as_ref(),
        target.tier.clone(),
        &target.sync,
        enforced,
        target.cooldown_secs,
    )
    .map_err(|e| format!("failed to write rule file: {e}"))?;

    list_rules()
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| "rule written but could not be re-read".to_string())
}

/// Remove a coaching rule file by id. Returns the rule's sync hosts so the
/// caller can clean up generated files for those hosts (see components::unsync_host).
pub(super) fn remove_rule(id: &str) -> Result<Vec<String>, String> {
    if !rule::is_valid_rule_id(id) {
        return Err(format!("invalid rule id '{id}'"));
    }
    let _lock = RulesLock::acquire(&rules_dir())
        .map_err(|e| format!("failed to acquire rules lock: {e}"))?;
    let path = rules_dir().join(format!("coaching-{id}.md"));
    if !path.exists() {
        return Err(format!("rule not found: {id}"));
    }
    let sync = list_rules()
        .into_iter()
        .find(|r| r.id == id)
        .map(|r| r.sync)
        .unwrap_or_default();
    std::fs::remove_file(&path).map_err(|e| format!("failed to remove rule file: {e}"))?;
    Ok(sync)
}

/// (id, body) pairs for every rule in the store that is tagged to sync
/// to `host`, in rule-id order. Callers (components::rule_targets) build
/// the actual per-host file path themselves.
pub fn sync_targets_for_host(host: &str) -> Vec<(String, String)> {
    list_rules()
        .into_iter()
        .filter(|r| r.sync.iter().any(|h| h == host))
        .map(|r| (r.id, r.body))
        .collect()
}

/// Appends `previous_body` to `~/.agentflare/rules/superseded-<id>.json`
/// (creating it if absent) — the coaching-store equivalent of
/// `rule_text::superseded()`'s hardcoded lists, built automatically
/// instead of hand-maintained, so `init::is_stale_rule` can tell "still
/// has a body we shipped before" from "user edited this" for builtin-tier
/// coaching rules the same way it already does for `rule_text.rs` consts.
fn snapshot_previous_body(id: &str, previous_body: &str) -> std::io::Result<()> {
    let path = rules_dir().join(format!("superseded-{id}.json"));
    let mut bodies: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if !bodies.iter().any(|b| b == previous_body) {
        bodies.push(previous_body.to_string());
    }
    std::fs::create_dir_all(rules_dir())?;
    std::fs::write(&path, serde_json::to_string(&bodies).unwrap_or_default())
}

/// Known old bodies for a coaching-sourced builtin rule, by id — the
/// coaching-store analogue of `rule_text::superseded(filename)`, built
/// automatically instead of hand-maintained, so `init::is_stale_rule` can
/// tell "still has a body we shipped before" from "user edited this".
pub fn superseded_bodies(id: &str) -> Vec<String> {
    let path = rules_dir().join(format!("superseded-{id}.json"));
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Bodies of rules with no Trigger declared, in id order, what
/// hook session-start surfaces unconditionally.
pub fn untriggered_rule_bodies() -> Vec<String> {
    list_rules()
        .into_iter()
        .filter(|r| r.trigger.is_none())
        .map(|r| r.body)
        .collect()
}

/// Bodies of rules whose trigger declares this exact tool name, paced so a
/// rule that already fired within its cooldown window is suppressed rather
/// than returned again.
pub fn rule_bodies_for_tool(tool_name: &str) -> Vec<String> {
    list_rules()
        .into_iter()
        .filter(|r| {
            r.trigger
                .as_ref()
                .is_some_and(|trigger| trigger.tools.iter().any(|t| t == tool_name))
        })
        .filter_map(|r| {
            let cooldown = r
                .cooldown_secs
                .map(std::time::Duration::from_secs)
                .unwrap_or(crate::nudge_pace::DEFAULT_COOLDOWN);
            let key = format!("coaching:{}", r.id);
            if crate::nudge_pace::should_fire(&key, cooldown) {
                crate::nudge_pace::mark_fired(&key);
                Some(r.body)
            } else {
                None
            }
        })
        .collect()
}

/// The "usetsearch" rule's own body says native ToolSearch is still right
/// for tools outside the flare gateway (WebFetch, EnterPlanMode,
/// mcp__claude-in-chrome__*, and agentflare's own first-party
/// `mcp__flare__*` tools, which are already in the deferred-tool list and
/// need no gateway detour) -- but until this check existed,
/// `enforced_rule_reason_for_tool` matched on tool_name alone and blocked
/// *every* ToolSearch call, contradicting that fallback. Only a query that's
/// actually hunting for a leanctx/`ctx_*` tool -- the one case genuinely
/// absent from the deferred-tool list, see CLAUDE.md's "mcp__lean-ctx__* is
/// not a tool namespace this session ever sees listed" -- gets redirected.
/// Spaces and hyphens are folded to underscores before matching so a query
/// like "ctx read" or "lean ctx" (no literal underscore) still counts --
/// the keyword list itself is written in that same folded form.
fn toolsearch_query_is_gateway_shaped(tool_input: Option<&Value>) -> bool {
    let query = tool_input
        .and_then(|v| v.get("query"))
        .and_then(|q| q.as_str())
        .unwrap_or_default()
        .to_lowercase()
        .replace([' ', '-'], "_");
    ["ctx_", "leanctx", "lean_ctx", "mcp__lean_ctx__"]
        .iter()
        .any(|kw| query.contains(kw))
}

/// Redirect-framed deny reason for the first MANDATORY (enforced) rule
/// whose trigger declares `tool_name`, if any. Distinct from
/// `rule_bodies_for_tool`'s advisory nudges: this is consulted by the
/// PreToolUse hook to block the call outright, not just remind about it.
/// `tool_input` lets rule-specific checks look past the tool name at what's
/// actually being asked for -- currently only "usetsearch" uses it (see
/// `toolsearch_query_is_gateway_shaped`); other enforced rules match on
/// tool_name alone, same as before. The content-check only applies to the
/// Builtin-tier "usetsearch" rule agentflare itself ships -- a user who has
/// overridden it (same id, tier Override) keeps the old tool-name-only
/// enforcement, per the coaching system's "override always wins" contract
/// (see `components.rs`'s `DefaultCoachingRule` doc comment): overriding a
/// rule's body must not leave a hardcoded content-filter still shadowing it.
pub fn enforced_rule_reason_for_tool(
    tool_name: &str,
    tool_input: Option<&Value>,
) -> Option<String> {
    list_rules()
        .into_iter()
        .find(|r| {
            if !r.enforced {
                return false;
            }
            if !r
                .trigger
                .as_ref()
                .is_some_and(|trigger| trigger.tools.iter().any(|t| t == tool_name))
            {
                return false;
            }
            if r.id == "usetsearch" && r.tier == RuleTier::Builtin {
                return toolsearch_query_is_gateway_shaped(tool_input);
            }
            true
        })
        .map(|r| {
            format!(
                "MANDATORY coaching rule '{}' redirects {tool_name} \u{2014} {}",
                r.title, r.body
            )
        })
}

/// Bodies of rules with auto_match true whose title+body BM25-matches
/// prompt, via the same ephemeral FTS5 scorer crate::compact already
/// built for the PreCompact hook. No numeric threshold: appearing in
/// score_lines's result set is itself the fire or no-fire decision. Paced
/// so a rule that already fired within its cooldown window is suppressed
/// rather than returned again.
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

    let scored = match crate::compact::score_lines(&entries, prompt) {
        Ok(scored) => scored,
        Err(_) => return vec![],
    };

    scored
        .into_iter()
        .filter_map(|s| {
            let r = &candidates[s.index];
            let cooldown = r
                .cooldown_secs
                .map(std::time::Duration::from_secs)
                .unwrap_or(crate::nudge_pace::DEFAULT_COOLDOWN);
            let key = format!("coaching:{}", r.id);
            if crate::nudge_pace::should_fire(&key, cooldown) {
                crate::nudge_pace::mark_fired(&key);
                Some(r.body.clone())
            } else {
                None
            }
        })
        .collect()
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
                RuleTier::Override,
                vec![],
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
            let err =
                apply_rule("1bad", "Title", "Body", None, RuleTier::Override, vec![]).unwrap_err();
            assert!(err.contains("invalid rule id"));
            assert!(list_rules().is_empty());
        });
    }

    #[test]
    fn apply_rule_rejects_newline_in_title() {
        with_temp_home(|| {
            let err = apply_rule(
                "hygiene",
                "bad\ntitle",
                "Body",
                None,
                RuleTier::Override,
                vec![],
            )
            .unwrap_err();
            assert!(err.contains("newline"));
            assert!(list_rules().is_empty());
        });
    }

    #[test]
    fn apply_rule_rejects_empty_trigger() {
        with_temp_home(|| {
            let empty = rule::RuleTrigger {
                tools: vec![],
                auto_match: false,
            };
            let err = apply_rule(
                "hygiene",
                "Title",
                "Body",
                Some(empty),
                RuleTier::Override,
                vec![],
            )
            .unwrap_err();
            assert!(err.contains("empty trigger"));
            assert!(list_rules().is_empty());
        });
    }

    #[test]
    fn apply_rule_enforces_max_rules_for_new_ids() {
        with_temp_home(|| {
            for i in 0..MAX_RULES {
                apply_rule(&format!("r{i}"), "T", "B", None, RuleTier::Override, vec![]).unwrap();
            }
            let err =
                apply_rule("one-more", "T", "B", None, RuleTier::Override, vec![]).unwrap_err();
            assert!(err.contains("maximum"));
            assert_eq!(list_rules().len(), MAX_RULES);
        });
    }

    #[test]
    fn apply_rule_allows_overwriting_existing_id_at_capacity() {
        with_temp_home(|| {
            for i in 0..MAX_RULES {
                apply_rule(&format!("r{i}"), "T", "B", None, RuleTier::Override, vec![]).unwrap();
            }
            let updated = apply_rule(
                "r0",
                "New Title",
                "New Body",
                None,
                RuleTier::Override,
                vec![],
            )
            .unwrap();
            assert_eq!(updated.title, "New Title");
            assert_eq!(list_rules().len(), MAX_RULES);
        });
    }

    #[test]
    fn remove_rule_deletes_existing_file() {
        with_temp_home(|| {
            apply_rule("hygiene", "T", "B", None, RuleTier::Override, vec![]).unwrap();
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
            apply_rule("good", "T", "B", None, RuleTier::Override, vec![]).unwrap();

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
            apply_rule(
                "b-rule",
                "Title B",
                "Body B",
                None,
                RuleTier::Override,
                vec![],
            )
            .unwrap();
            apply_rule(
                "a-rule",
                "Title A",
                "Body A",
                None,
                RuleTier::Override,
                vec![],
            )
            .unwrap();
            assert_eq!(
                untriggered_rule_bodies(),
                vec!["Body A".to_string(), "Body B".to_string()]
            );
        });
    }

    #[test]
    fn apply_rule_body_with_dashes_line_is_not_truncated() {
        with_temp_home(|| {
            let applied = apply_rule(
                "dashes",
                "Title",
                "before\n---\nafter",
                None,
                RuleTier::Override,
                vec![],
            )
            .unwrap();
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
            let applied = apply_rule(
                "emdash",
                "Foo \u{2014} Bar",
                "Body",
                None,
                RuleTier::Override,
                vec![],
            )
            .unwrap();
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
            let applied = apply_rule(
                "revfix",
                "Title",
                "Body",
                Some(trigger.clone()),
                RuleTier::Override,
                vec![],
            )
            .unwrap();
            assert_eq!(applied.trigger, Some(trigger.clone()));

            let rules = list_rules();
            assert_eq!(rules.len(), 1);
            assert_eq!(rules[0].trigger, Some(trigger));
        });
    }

    #[test]
    fn apply_rule_without_trigger_is_untriggered() {
        with_temp_home(|| {
            let applied =
                apply_rule("hygiene", "Title", "Body", None, RuleTier::Override, vec![]).unwrap();
            assert_eq!(applied.trigger, None);
        });
    }

    #[test]
    fn untriggered_rule_bodies_excludes_triggered_rules() {
        with_temp_home(|| {
            apply_rule("plain", "T", "Plain body", None, RuleTier::Override, vec![]).unwrap();
            apply_rule(
                "scoped",
                "T",
                "Scoped body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
                RuleTier::Override,
                vec![],
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
                RuleTier::Override,
                vec![],
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
                RuleTier::Override,
                vec![],
            )
            .unwrap();

            let bodies = rule_bodies_for_prompt("please review this change");
            assert_eq!(
                bodies,
                vec!["Every review finding needs a diff.".to_string()]
            );
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
                RuleTier::Override,
                vec![],
            )
            .unwrap();

            assert!(rule_bodies_for_prompt("sunny weather forecast tomorrow morning").is_empty());
        });
    }

    #[test]
    fn rule_bodies_for_prompt_ignores_non_auto_match_rules() {
        with_temp_home(|| {
            apply_rule("plain", "T", "Plain body", None, RuleTier::Override, vec![]).unwrap();
            apply_rule(
                "tool-only",
                "T",
                "Tool only body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
                RuleTier::Override,
                vec![],
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

    #[test]
    fn apply_rule_snapshots_previous_body_when_overwriting_a_builtin_rule() {
        with_temp_home(|| {
            apply_rule(
                "search17",
                "T",
                "Old body",
                None,
                rule::RuleTier::Builtin,
                vec!["claude-code".to_string()],
            )
            .unwrap();
            apply_rule(
                "search17",
                "T",
                "New body",
                None,
                rule::RuleTier::Builtin,
                vec!["claude-code".to_string()],
            )
            .unwrap();

            let path = rules_dir().join("superseded-search17.json");
            let bodies: Vec<String> =
                serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
            assert_eq!(bodies, vec!["Old body".to_string()]);
        });
    }

    #[test]
    fn apply_rule_does_not_snapshot_when_overwriting_an_override_tier_rule() {
        with_temp_home(|| {
            apply_rule(
                "hygiene",
                "T",
                "Old body",
                None,
                rule::RuleTier::Override,
                vec![],
            )
            .unwrap();
            apply_rule(
                "hygiene",
                "T",
                "New body",
                None,
                rule::RuleTier::Override,
                vec![],
            )
            .unwrap();
            assert!(!rules_dir().join("superseded-hygiene.json").exists());
        });
    }

    #[test]
    fn apply_rule_does_not_snapshot_when_body_is_unchanged() {
        with_temp_home(|| {
            apply_rule(
                "search17",
                "T",
                "Same body",
                None,
                rule::RuleTier::Builtin,
                vec![],
            )
            .unwrap();
            apply_rule(
                "search17",
                "T",
                "Same body",
                None,
                rule::RuleTier::Builtin,
                vec![],
            )
            .unwrap();
            assert!(!rules_dir().join("superseded-search17.json").exists());
        });
    }

    #[test]
    fn set_enforced_flips_flag_on_existing_rule() {
        with_temp_home(|| {
            apply_rule(
                "revfix",
                "T",
                "Body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
                RuleTier::Override,
                vec![],
            )
            .unwrap();

            let updated = set_enforced("revfix", true).unwrap();
            assert!(updated.enforced);

            let rules = list_rules();
            assert!(rules[0].enforced);
        });
    }

    #[test]
    fn set_enforced_rejects_rule_without_tool_trigger() {
        with_temp_home(|| {
            apply_rule("hygiene", "T", "Body", None, RuleTier::Override, vec![]).unwrap();

            let err = set_enforced("hygiene", true).unwrap_err();
            assert!(err.contains("no tool trigger"));
        });
    }

    #[test]
    fn set_enforced_errors_when_rule_not_found() {
        with_temp_home(|| {
            let err = set_enforced("missing", true).unwrap_err();
            assert!(err.contains("not found"));
        });
    }

    #[test]
    fn apply_rule_preserves_enforced_flag_across_body_refresh() {
        with_temp_home(|| {
            apply_rule(
                "revfix",
                "T",
                "Old body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
                RuleTier::Builtin,
                vec![],
            )
            .unwrap();
            set_enforced("revfix", true).unwrap();

            apply_rule(
                "revfix",
                "T",
                "New body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
                RuleTier::Builtin,
                vec![],
            )
            .unwrap();

            let rules = list_rules();
            assert_eq!(rules[0].body, "New body");
            assert!(
                rules[0].enforced,
                "enforced flag must survive a body refresh"
            );
        });
    }

    #[test]
    fn apply_rule_preserves_cooldown_secs_across_plain_refresh() {
        with_temp_home(|| {
            apply_rule_with_cooldown(
                "hygiene",
                "T",
                "Old body",
                None,
                RuleTier::Override,
                vec![],
                Some(600),
            )
            .unwrap();

            apply_rule("hygiene", "T", "New body", None, RuleTier::Override, vec![]).unwrap();

            let rules = list_rules();
            assert_eq!(rules[0].body, "New body");
            assert_eq!(
                rules[0].cooldown_secs,
                Some(600),
                "cooldown_secs must survive a plain apply_rule refresh, same as enforced does"
            );
        });
    }

    #[test]
    fn enforced_rule_reason_for_tool_only_fires_for_enforced_rules() {
        with_temp_home(|| {
            apply_rule(
                "revfix",
                "Reviews ship with fixes",
                "Every finding needs a diff.",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
                RuleTier::Override,
                vec![],
            )
            .unwrap();

            assert!(enforced_rule_reason_for_tool("mcp__flare__review", None).is_none());

            set_enforced("revfix", true).unwrap();
            let reason = enforced_rule_reason_for_tool("mcp__flare__review", None).unwrap();
            assert!(reason.contains("MANDATORY"));
            assert!(reason.contains("Every finding needs a diff."));

            assert!(enforced_rule_reason_for_tool("mcp__flare__comment", None).is_none());
        });
    }

    #[test]
    fn toolsearch_only_enforces_for_gateway_shaped_queries() {
        with_temp_home(|| {
            apply_rule(
                "usetsearch",
                "flare tool-search over native ToolSearch for gateway tools",
                "Use the gateway search instead.",
                Some(rule::RuleTrigger {
                    tools: vec!["ToolSearch".to_string()],
                    auto_match: false,
                }),
                RuleTier::Builtin,
                vec![],
            )
            .unwrap();
            set_enforced("usetsearch", true).unwrap();

            // A leanctx/ctx_* lookup is the one case genuinely missing from
            // the deferred-tool list -- still blocked and redirected.
            let leanctx_query = serde_json::json!({"query": "ctx_read"});
            assert!(enforced_rule_reason_for_tool("ToolSearch", Some(&leanctx_query)).is_some());

            // First-party mcp__flare__* tools are already in the deferred
            // list -- native ToolSearch must be allowed through untouched.
            let flare_query = serde_json::json!({"query": "select:mcp__flare__item"});
            assert!(enforced_rule_reason_for_tool("ToolSearch", Some(&flare_query)).is_none());

            // Tools entirely outside the gateway (WebFetch, etc.) must also
            // pass through unblocked.
            let webfetch_query = serde_json::json!({"query": "select:WebFetch"});
            assert!(enforced_rule_reason_for_tool("ToolSearch", Some(&webfetch_query)).is_none());

            // No query at all (malformed input) fails closed to "allow" --
            // never blocks based on absent data.
            assert!(enforced_rule_reason_for_tool("ToolSearch", None).is_none());

            // Space/hyphen variants of the same keywords still match --
            // a human is far more likely to type "ctx read" or "lean ctx"
            // than the literal underscored tool name.
            let spaced_query = serde_json::json!({"query": "ctx read tool"});
            assert!(enforced_rule_reason_for_tool("ToolSearch", Some(&spaced_query)).is_some());
            let lean_ctx_query = serde_json::json!({"query": "lean ctx tools"});
            assert!(enforced_rule_reason_for_tool("ToolSearch", Some(&lean_ctx_query)).is_some());
        });
    }

    #[test]
    fn toolsearch_override_skips_content_filter_and_enforces_on_tool_name_alone() {
        with_temp_home(|| {
            // A user-authored override of "usetsearch" (same id, tier
            // Override) must win outright -- the content filter is a
            // Builtin-tier-only behavior, per "override always wins".
            apply_rule(
                "usetsearch",
                "my own ToolSearch policy",
                "Never use ToolSearch, period.",
                Some(rule::RuleTrigger {
                    tools: vec!["ToolSearch".to_string()],
                    auto_match: false,
                }),
                RuleTier::Override,
                vec![],
            )
            .unwrap();
            set_enforced("usetsearch", true).unwrap();

            // Even a query that isn't gateway-shaped at all is still
            // blocked, because the override's own (unconditional) intent
            // -- not the builtin content filter -- governs.
            let webfetch_query = serde_json::json!({"query": "select:WebFetch"});
            let reason =
                enforced_rule_reason_for_tool("ToolSearch", Some(&webfetch_query)).unwrap();
            assert!(reason.contains("my own ToolSearch policy"));
        });
    }

    #[test]
    fn rule_bodies_for_tool_is_suppressed_on_an_immediate_second_call() {
        with_temp_home(|| {
            apply_rule(
                "scoped",
                "T",
                "Scoped body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
                RuleTier::Override,
                vec![],
            )
            .unwrap();

            assert_eq!(
                rule_bodies_for_tool("mcp__flare__review"),
                vec!["Scoped body".to_string()]
            );
            assert!(
                rule_bodies_for_tool("mcp__flare__review").is_empty(),
                "second call within the cooldown must be paced"
            );
        });
    }

    #[test]
    fn rule_bodies_for_tool_honors_a_custom_cooldown_not_just_the_default() {
        with_temp_home(|| {
            apply_rule_with_cooldown(
                "scoped",
                "T",
                "Scoped body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
                RuleTier::Override,
                vec![],
                Some(0),
            )
            .unwrap();

            // cooldown_secs = 0 opts a rule out of pacing entirely -- if
            // rule_bodies_for_tool ignored per-rule cooldown_secs and always
            // used DEFAULT_COOLDOWN (300s), the second call below would
            // wrongly come back empty.
            assert_eq!(
                rule_bodies_for_tool("mcp__flare__review"),
                vec!["Scoped body".to_string()]
            );
            assert_eq!(
                rule_bodies_for_tool("mcp__flare__review"),
                vec!["Scoped body".to_string()],
                "an explicit 0s cooldown must not be overridden by DEFAULT_COOLDOWN"
            );
        });
    }

    #[test]
    fn rule_bodies_for_prompt_is_suppressed_on_an_immediate_second_call() {
        with_temp_home(|| {
            apply_rule(
                "revfix",
                "Reviews ship with fixes",
                "Every review finding needs a diff.",
                Some(rule::RuleTrigger {
                    tools: vec![],
                    auto_match: true,
                }),
                RuleTier::Override,
                vec![],
            )
            .unwrap();

            assert_eq!(
                rule_bodies_for_prompt("please review this change"),
                vec!["Every review finding needs a diff.".to_string()]
            );
            assert!(
                rule_bodies_for_prompt("please review this change").is_empty(),
                "second call within the cooldown must be paced"
            );
        });
    }

    #[test]
    fn enforced_rule_reason_for_tool_is_never_paced() {
        with_temp_home(|| {
            apply_rule(
                "revfix",
                "T",
                "Body",
                Some(rule::RuleTrigger {
                    tools: vec!["mcp__flare__review".to_string()],
                    auto_match: false,
                }),
                RuleTier::Override,
                vec![],
            )
            .unwrap();
            set_enforced("revfix", true).unwrap();

            assert!(enforced_rule_reason_for_tool("mcp__flare__review").is_some());
            assert!(
                enforced_rule_reason_for_tool("mcp__flare__review").is_some(),
                "enforced rules must fire on every matching call, never paced"
            );
        });
    }
}
