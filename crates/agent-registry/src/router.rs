// Task -> agent routing: the "which one to pick" piece the other three
// (which agents exist, which are installed, how to run one) didn't have.
//
// Distinct from `optimize::runtime::Router` in the main crate, which returns
// a prose nudge for a single LLM call from prompt text. This one's `route()`
// returns a `RouteDecision` for a claimed task from task attributes (labels,
// kind, size) — durable and inspectable, unlike prompt wording. Named
// `TaskContext`/`RouteDecision`/`RouterConfig` rather than reusing
// `Router`/`RouteContext` so the two don't collide.
use crate::registry::{Agent, agent_by_name};
use std::collections::HashMap;

/// Attributes of a claimed task available to route on.
#[derive(Debug, Clone, Default)]
pub struct TaskContext {
    pub labels: Vec<String>,
    pub kind: Option<String>,
    pub size: Option<String>,
    pub repo: Option<String>,
    /// A human (or a prior process) already named an agent for this task —
    /// always wins over every rule and the default.
    pub assigned_agent: Option<Agent>,
    /// The SDD pipeline stage this route is for (`"implementer"`,
    /// `"judge"`, ...) — lets one `[router]` config assign a different
    /// agent/model per role of the same work item instead of one agent
    /// playing every role (see `work_item_pipeline::build_sdd_loop_step`,
    /// which already takes an independent agent per role but had nothing
    /// upstream populating them differently until this field existed).
    /// `None` for callers that route a whole task as one unit.
    pub role: Option<String>,
}

/// What a route decided, and why. The reason is what a dashboard would show
/// and what a test asserts against — a decision that can't explain itself
/// can't be debugged or trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteDecision {
    pub agent: Agent,
    pub model: Option<String>,
    pub reason: String,
}

/// The task attributes a rule matches on. Empty (all fields unset) matches
/// nothing — a rule with nothing to say about a task should not fire for
/// every task.
#[derive(Debug, Clone, Default)]
pub struct RuleMatch {
    pub labels: Vec<String>,
    pub kind: Option<String>,
    pub size: Option<String>,
    /// Matches `TaskContext::role` (`"implementer"`, `"judge"`, ...) —
    /// unset matches any role, same as every other field here.
    pub role: Option<String>,
}

impl RuleMatch {
    fn is_empty(&self) -> bool {
        self.labels.is_empty() && self.kind.is_none() && self.size.is_none() && self.role.is_none()
    }

    fn matches(&self, task: &TaskContext) -> bool {
        if self.is_empty() {
            return false;
        }
        let labels_ok = self.labels.iter().all(|want| {
            task.labels
                .iter()
                .any(|have| have.eq_ignore_ascii_case(want))
        });
        let matches_field = |want: &Option<String>, have: &Option<String>| {
            want.is_none()
                || match (want, have) {
                    (Some(w), Some(h)) => w.eq_ignore_ascii_case(h),
                    _ => false,
                }
        };
        labels_ok
            && matches_field(&self.kind, &task.kind)
            && matches_field(&self.size, &task.size)
            && matches_field(&self.role, &task.role)
    }
}

/// One user-configured routing rule: fire on `when`, prefer the first
/// installed agent in `use_agents` — unless `rotate` is set, in which case
/// `route()` round-robins across every installed agent in `use_agents`
/// instead, splitting matching tasks across all of them rather than always
/// preferring the first.
#[derive(Debug, Clone)]
pub struct RouterRule {
    pub when: RuleMatch,
    pub use_agents: Vec<Agent>,
    pub rotate: bool,
    /// Model to request from whichever agent this rule picks (passed
    /// through as `RouteDecision::model`) — e.g. a role-specific rule can
    /// pin the judge to a stronger model than the implementer without
    /// needing a different agent. `None` leaves the caller's own default.
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RouterConfig {
    pub default: Option<Agent>,
    pub rules: Vec<RouterRule>,
}

/// A single string or an array of strings — TOML doesn't have a "one or
/// many" type, so `use = "codex"` and `use = ["codex", "opencode"]` both
/// need to deserialize into the same `Vec<String>` preference list.
#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawWhen {
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    role: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawRule {
    #[serde(default)]
    when: RawWhen,
    #[serde(rename = "use")]
    use_agents: OneOrMany,
    #[serde(default)]
    rotate: bool,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawRouterConfig {
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    rule: Vec<RawRule>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawFile {
    #[serde(default)]
    router: Option<RawRouterConfig>,
}

/// Parses a `~/.agentflare/config.toml`-shaped document down to its
/// `[router]` table, ignoring every other top-level table — the file is
/// #331's surface, this only ever reads its own corner of it. Returns the
/// empty (no rules, no default) `RouterConfig` when `[router]` is absent
/// entirely, e.g. the file doesn't exist yet.
///
/// An agent name that doesn't resolve (typo, or an agent this build's
/// registry doesn't know) is dropped from the preference list it appeared
/// in rather than failing the whole parse — `route()` already treats a
/// dropped/not-installed preference as "try the next one", so a bad name
/// behaves exactly like one that's merely not installed. Only malformed
/// TOML syntax is a hard `Err`.
pub fn parse_router_config(text: &str) -> Result<RouterConfig, String> {
    let file: RawFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let Some(raw) = file.router else {
        return Ok(RouterConfig::default());
    };
    let default = raw.default.as_deref().and_then(agent_by_name);
    let rules = raw
        .rule
        .into_iter()
        .map(|r| RouterRule {
            when: RuleMatch {
                labels: r.when.labels,
                kind: r.when.kind,
                size: r.when.size,
                role: r.when.role,
            },
            use_agents: r
                .use_agents
                .into_vec()
                .iter()
                .filter_map(|s| agent_by_name(s))
                .collect(),
            rotate: r.rotate,
            model: r.model,
        })
        .collect();
    Ok(RouterConfig { default, rules })
}

/// The model configured by the first `[router]` rule matching `task`,
/// independent of whether that rule's own `use_agents` is installed or even
/// relevant — for callers where the agent is already fixed (an explicit
/// assignment, or an operator's `--agent` flag) and only a rule's `model`
/// still needs to apply. Mirrors `route()`'s first-match-wins precedence: a
/// matching rule with no `model` set yields `None`, even if a later rule
/// would have one.
#[must_use]
pub fn model_for_task(task: &TaskContext, config: &RouterConfig) -> Option<String> {
    config
        .rules
        .iter()
        .find(|rule| rule.when.matches(task))
        .and_then(|rule| rule.model.clone())
}

/// Decides which agent should run `task`, or `None` if nothing local fits —
/// the caller's cue to fall back (e.g. cede via the bridge).
///
/// Precedence, most specific first: explicit assignment on the task beats
/// every rule; the first matching rule with an installed preference beats
/// the default; the default only fires if it is itself installed.
/// `installed` (`detect_all()`'s output, by agent id) is the authority on
/// availability for rules and the default — either falls through rather
/// than hard-failing or silently picking whatever the caller didn't ask
/// for, if the name they resolve to isn't installed. Explicit assignment is
/// the one exception: "a human naming an agent always wins" means it is
/// returned without an `installed` check, on the theory that a person who
/// named an agent knows something the detector doesn't (e.g. it's about to
/// be installed) — an assignment that's wrong instead fails downstream
/// (e.g. `run_headless`'s `NotFound`) with a clearer error than a silent
/// route-level `None` would give.
///
/// `rotation` persists round-robin position across calls for rules with
/// `rotate = true` — keyed by the rule's index in `config.rules` (`"rule_
/// <idx>"`), so it stays stable as long as the caller's config file's rule
/// order doesn't change. Callers that never use `rotate` can pass an empty,
/// throwaway map; `route()` never reads a key it didn't itself write.
#[must_use]
pub fn route(
    task: &TaskContext,
    config: &RouterConfig,
    installed: &[Agent],
    rotation: &mut HashMap<String, u64>,
) -> Option<RouteDecision> {
    if let Some(agent) = task.assigned_agent {
        return Some(RouteDecision {
            agent,
            model: model_for_task(task, config),
            reason: "explicit assignment on task".to_string(),
        });
    }

    for (idx, rule) in config.rules.iter().enumerate() {
        if !rule.when.matches(task) {
            continue;
        }
        let candidates: Vec<Agent> = rule
            .use_agents
            .iter()
            .copied()
            .filter(|a| installed.contains(a))
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let agent = if rule.rotate && candidates.len() > 1 {
            let counter = rotation.entry(format!("rule_{idx}")).or_insert(0);
            let picked = candidates[(*counter as usize) % candidates.len()];
            *counter = counter.wrapping_add(1);
            picked
        } else {
            candidates[0]
        };
        let reason = if rule.rotate && candidates.len() > 1 {
            format!(
                "round-robin among {candidates:?} (labels={:?}, kind={:?}, size={:?})",
                rule.when.labels, rule.when.kind, rule.when.size
            )
        } else {
            format!(
                "matched rule (labels={:?}, kind={:?}, size={:?})",
                rule.when.labels, rule.when.kind, rule.when.size
            )
        };
        return Some(RouteDecision {
            agent,
            model: rule.model.clone(),
            reason,
        });
    }

    let default = config.default?;
    installed.contains(&default).then_some(RouteDecision {
        agent: default,
        model: None,
        reason: "default".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(labels: &[&str], use_agents: &[Agent]) -> RouterRule {
        RouterRule {
            when: RuleMatch {
                labels: labels.iter().map(|s| s.to_string()).collect(),
                kind: None,
                size: None,
                role: None,
            },
            use_agents: use_agents.to_vec(),
            rotate: false,
            model: None,
        }
    }

    fn rotating_rule(labels: &[&str], use_agents: &[Agent]) -> RouterRule {
        RouterRule {
            rotate: true,
            ..rule(labels, use_agents)
        }
    }

    fn role_rule(role: &str, use_agents: &[Agent]) -> RouterRule {
        RouterRule {
            when: RuleMatch {
                role: Some(role.to_string()),
                ..Default::default()
            },
            ..rule(&[], use_agents)
        }
    }

    fn no_rotation() -> HashMap<String, u64> {
        HashMap::new()
    }

    #[test]
    fn explicit_assignment_wins_over_everything_even_when_not_installed() {
        let task = TaskContext {
            assigned_agent: Some(Agent::Codex),
            ..Default::default()
        };
        let config = RouterConfig {
            default: Some(Agent::ClaudeCode),
            rules: vec![rule(&[], &[Agent::Opencode])],
        };
        // Codex is deliberately absent from `installed` here: an explicit
        // assignment is the one path that bypasses the installed check.
        let decision = route(&task, &config, &[Agent::ClaudeCode], &mut no_rotation()).unwrap();
        assert_eq!(decision.agent, Agent::Codex);
        assert_eq!(decision.reason, "explicit assignment on task");
        assert_eq!(decision.model, None);
    }

    #[test]
    fn explicit_assignment_still_picks_up_a_matching_rules_model() {
        // Item #162: an assigned agent used to make `route()` bail with
        // `model: None` before ever consulting `[router]` rules — which
        // meant a rule's `model` could never reach a daemon-dispatched item
        // (every daemon dispatch carries an explicit `assigned_agent`).
        let task = TaskContext {
            labels: vec!["docs".to_string()],
            assigned_agent: Some(Agent::Cline),
            ..Default::default()
        };
        let config = RouterConfig {
            default: None,
            rules: vec![RouterRule {
                model: Some("sonnet".to_string()),
                ..rule(&["docs"], &[Agent::Opencode])
            }],
        };
        // Opencode (the rule's `use`) isn't even installed — the matched
        // rule's model should still apply, since the agent is already fixed
        // by the assignment and the rule isn't being consulted for agent
        // selection here.
        let decision = route(&task, &config, &[Agent::ClaudeCode], &mut no_rotation()).unwrap();
        assert_eq!(decision.agent, Agent::Cline);
        assert_eq!(decision.model.as_deref(), Some("sonnet"));
    }

    #[test]
    fn matching_rule_picks_first_installed_preference() {
        let task = TaskContext {
            labels: vec!["docs".to_string()],
            ..Default::default()
        };
        let config = RouterConfig {
            default: None,
            rules: vec![rule(&["docs"], &[Agent::Codex, Agent::Opencode])],
        };
        // Codex not installed, Opencode is -> second preference wins.
        let decision = route(&task, &config, &[Agent::Opencode], &mut no_rotation()).unwrap();
        assert_eq!(decision.agent, Agent::Opencode);
    }

    #[test]
    fn rule_requires_all_its_labels_present_extra_task_labels_ok() {
        let task = TaskContext {
            labels: vec![
                "security".to_string(),
                "auth".to_string(),
                "urgent".to_string(),
            ],
            ..Default::default()
        };
        let config = RouterConfig {
            default: None,
            rules: vec![rule(&["security", "auth"], &[Agent::ClaudeCode])],
        };
        assert!(route(&task, &config, &[Agent::ClaudeCode], &mut no_rotation()).is_some());
    }

    #[test]
    fn rule_with_none_of_its_preferences_installed_falls_through_to_next_rule() {
        let task = TaskContext {
            labels: vec!["docs".to_string()],
            ..Default::default()
        };
        let config = RouterConfig {
            default: None,
            rules: vec![
                rule(&["docs"], &[Agent::Codex]),
                rule(&["docs"], &[Agent::Opencode]),
            ],
        };
        let decision = route(&task, &config, &[Agent::Opencode], &mut no_rotation()).unwrap();
        assert_eq!(decision.agent, Agent::Opencode);
    }

    #[test]
    fn empty_rule_never_matches() {
        let task = TaskContext::default();
        let config = RouterConfig {
            default: None,
            rules: vec![RouterRule {
                when: RuleMatch::default(),
                use_agents: vec![Agent::ClaudeCode],
                rotate: false,
                model: None,
            }],
        };
        assert!(route(&task, &config, &[Agent::ClaudeCode], &mut no_rotation()).is_none());
    }

    #[test]
    fn kind_and_size_must_match_when_specified() {
        let rule = RouterRule {
            when: RuleMatch {
                labels: vec![],
                kind: Some("locate".to_string()),
                size: Some("S".to_string()),
                role: None,
            },
            use_agents: vec![Agent::Opencode],
            rotate: false,
            model: None,
        };
        let config = RouterConfig {
            default: None,
            rules: vec![rule],
        };

        let matching = TaskContext {
            kind: Some("locate".to_string()),
            size: Some("S".to_string()),
            ..Default::default()
        };
        assert!(route(&matching, &config, &[Agent::Opencode], &mut no_rotation()).is_some());

        let wrong_size = TaskContext {
            kind: Some("locate".to_string()),
            size: Some("L".to_string()),
            ..Default::default()
        };
        assert!(route(&wrong_size, &config, &[Agent::Opencode], &mut no_rotation()).is_none());
    }

    #[test]
    fn default_used_when_no_rule_matches_and_default_installed() {
        let task = TaskContext::default();
        let config = RouterConfig {
            default: Some(Agent::ClaudeCode),
            rules: vec![],
        };
        let decision = route(&task, &config, &[Agent::ClaudeCode], &mut no_rotation()).unwrap();
        assert_eq!(decision.agent, Agent::ClaudeCode);
        assert_eq!(decision.reason, "default");
    }

    #[test]
    fn default_not_installed_yields_no_decision_rather_than_a_wrong_pick() {
        let task = TaskContext::default();
        let config = RouterConfig {
            default: Some(Agent::ClaudeCode),
            rules: vec![],
        };
        assert!(route(&task, &config, &[Agent::Opencode], &mut no_rotation()).is_none());
    }

    #[test]
    fn rotating_rule_alternates_across_installed_candidates() {
        let task = TaskContext {
            labels: vec!["implementor".to_string()],
            ..Default::default()
        };
        let config = RouterConfig {
            default: None,
            rules: vec![rotating_rule(
                &["implementor"],
                &[Agent::Opencode, Agent::Cursor],
            )],
        };
        let installed = [Agent::Opencode, Agent::Cursor];
        let mut rotation = no_rotation();

        let first = route(&task, &config, &installed, &mut rotation)
            .unwrap()
            .agent;
        let second = route(&task, &config, &installed, &mut rotation)
            .unwrap()
            .agent;
        let third = route(&task, &config, &installed, &mut rotation)
            .unwrap()
            .agent;

        assert_eq!(first, Agent::Opencode);
        assert_eq!(second, Agent::Cursor);
        assert_eq!(third, Agent::Opencode, "counter wraps back around");
    }

    #[test]
    fn rotating_rule_with_one_installed_candidate_always_picks_it() {
        let task = TaskContext {
            labels: vec!["implementor".to_string()],
            ..Default::default()
        };
        let config = RouterConfig {
            default: None,
            rules: vec![rotating_rule(
                &["implementor"],
                &[Agent::Opencode, Agent::Cursor],
            )],
        };
        // Only Opencode installed -> nothing to rotate between.
        let mut rotation = no_rotation();
        let decision = route(&task, &config, &[Agent::Opencode], &mut rotation).unwrap();
        assert_eq!(decision.agent, Agent::Opencode);
    }

    #[test]
    fn non_rotating_rule_ignores_rotation_state() {
        let task = TaskContext {
            labels: vec!["docs".to_string()],
            ..Default::default()
        };
        let config = RouterConfig {
            default: None,
            rules: vec![rule(&["docs"], &[Agent::Opencode, Agent::Cursor])],
        };
        let installed = [Agent::Opencode, Agent::Cursor];
        let mut rotation = no_rotation();
        for _ in 0..3 {
            let decision = route(&task, &config, &installed, &mut rotation).unwrap();
            assert_eq!(
                decision.agent,
                Agent::Opencode,
                "always the first preference"
            );
        }
    }

    #[test]
    fn role_specific_rules_pick_independent_agents_for_the_same_task() {
        // Same underlying task (e.g. one SDD work item), routed once per
        // role — implementer rotates across two cheap agents, judge always
        // gets the one configured, stronger agent.
        let config = RouterConfig {
            default: None,
            rules: vec![
                role_rule("implementer", &[Agent::Opencode, Agent::Cursor]),
                role_rule("judge", &[Agent::ClaudeCode]),
            ],
        };
        let installed = [Agent::Opencode, Agent::Cursor, Agent::ClaudeCode];
        let mut rotation = no_rotation();

        let implementer_task = TaskContext {
            role: Some("implementer".to_string()),
            ..Default::default()
        };
        let judge_task = TaskContext {
            role: Some("judge".to_string()),
            ..Default::default()
        };

        let implementer = route(&implementer_task, &config, &installed, &mut rotation)
            .unwrap()
            .agent;
        let judge = route(&judge_task, &config, &installed, &mut rotation)
            .unwrap()
            .agent;

        assert_eq!(implementer, Agent::Opencode);
        assert_eq!(judge, Agent::ClaudeCode);
    }

    #[test]
    fn role_mismatch_falls_through_to_next_rule() {
        let config = RouterConfig {
            default: None,
            rules: vec![
                role_rule("judge", &[Agent::ClaudeCode]),
                rule(&[], &[Agent::Opencode]), // placeholder, never matches (empty when)
            ],
        };
        let task = TaskContext {
            role: Some("implementer".to_string()),
            ..Default::default()
        };
        assert!(
            route(&task, &config, &[Agent::ClaudeCode], &mut no_rotation()).is_none(),
            "a rule scoped to a different role must not fire"
        );
    }

    #[test]
    fn rule_model_passes_through_to_the_route_decision() {
        let config = RouterConfig {
            default: None,
            rules: vec![RouterRule {
                model: Some("claude-opus-4-8".to_string()),
                ..role_rule("judge", &[Agent::ClaudeCode])
            }],
        };
        let task = TaskContext {
            role: Some("judge".to_string()),
            ..Default::default()
        };
        let decision = route(&task, &config, &[Agent::ClaudeCode], &mut no_rotation()).unwrap();
        assert_eq!(decision.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn no_rules_no_default_yields_no_decision() {
        let task = TaskContext::default();
        let config = RouterConfig::default();
        assert!(route(&task, &config, &[Agent::ClaudeCode], &mut no_rotation()).is_none());
    }

    // ---- parse_router_config ----

    #[test]
    fn parse_reads_default_and_rules_from_the_spec_example() {
        let text = r#"
[router]
default = "claude-code"

[[router.rule]]
when = { labels = ["security", "auth"] }
use  = "claude-code"

[[router.rule]]
when = { kind = "locate", size = "S" }
use  = "opencode"

[[router.rule]]
when = { labels = ["docs"] }
use  = ["codex", "opencode"]
"#;
        let config = parse_router_config(text).unwrap();
        assert_eq!(config.default, Some(Agent::ClaudeCode));
        assert_eq!(config.rules.len(), 3);
        assert_eq!(config.rules[0].when.labels, vec!["security", "auth"]);
        assert_eq!(config.rules[0].use_agents, vec![Agent::ClaudeCode]);
        assert_eq!(config.rules[1].when.kind.as_deref(), Some("locate"));
        assert_eq!(config.rules[1].when.size.as_deref(), Some("S"));
        assert_eq!(
            config.rules[2].use_agents,
            vec![Agent::Codex, Agent::Opencode],
            "a `use` array preserves preference order"
        );
    }

    #[test]
    fn parse_reads_role_and_model_from_a_rule() {
        let text = r#"
[router]

[[router.rule]]
when = { role = "judge" }
use  = "claude-code"
model = "claude-opus-4-8"
"#;
        let config = parse_router_config(text).unwrap();
        assert_eq!(config.rules[0].when.role.as_deref(), Some("judge"));
        assert_eq!(config.rules[0].model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn parse_defaults_to_empty_config_when_router_table_is_absent() {
        let config = parse_router_config("[some_other_feature]\nkey = 1\n").unwrap();
        assert!(config.default.is_none());
        assert!(config.rules.is_empty());
    }

    #[test]
    fn parse_defaults_to_empty_config_on_an_empty_file() {
        let config = parse_router_config("").unwrap();
        assert!(config.default.is_none());
        assert!(config.rules.is_empty());
    }

    #[test]
    fn parse_drops_unknown_agent_names_instead_of_failing() {
        let text = r#"
[router]
default = "not-a-real-agent"

[[router.rule]]
when = { labels = ["docs"] }
use  = ["not-a-real-agent", "opencode"]
"#;
        let config = parse_router_config(text).unwrap();
        assert!(
            config.default.is_none(),
            "unknown default is dropped, not errored"
        );
        assert_eq!(
            config.rules[0].use_agents,
            vec![Agent::Opencode],
            "unknown name filtered out, known one kept"
        );
    }

    #[test]
    fn parse_reads_rotate_flag_defaulting_to_false() {
        let text = r#"
[router]

[[router.rule]]
when = { labels = ["implementor"] }
use  = ["opencode", "cursor"]
rotate = true

[[router.rule]]
when = { labels = ["docs"] }
use  = "opencode"
"#;
        let config = parse_router_config(text).unwrap();
        assert!(config.rules[0].rotate);
        assert!(!config.rules[1].rotate, "rotate defaults to false");
    }

    #[test]
    fn parse_rejects_malformed_toml() {
        assert!(parse_router_config("this is not [ valid toml").is_err());
    }
}
