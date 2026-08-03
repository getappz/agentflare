// Task -> agent routing: the "which one to pick" piece the other three
// (which agents exist, which are installed, how to run one) didn't have.
//
// Distinct from `optimize::runtime::Router` in the main crate, which returns
// a prose nudge for a single LLM call from prompt text. This one returns a
// `RouteDecision` for a claimed task from task attributes (labels, kind,
// size) — durable and inspectable, unlike prompt wording. Named `TaskRouter`
// rather than `Router` so the two don't collide.
use crate::registry::Agent;

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
}

impl RuleMatch {
    fn is_empty(&self) -> bool {
        self.labels.is_empty() && self.kind.is_none() && self.size.is_none()
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
        let kind_ok = self.kind.is_none() || self.kind.as_deref() == task.kind.as_deref();
        let size_ok = self.size.is_none() || self.size.as_deref() == task.size.as_deref();
        labels_ok && kind_ok && size_ok
    }
}

/// One user-configured routing rule: fire on `when`, prefer the first
/// installed agent in `use_agents`.
#[derive(Debug, Clone)]
pub struct RouterRule {
    pub when: RuleMatch,
    pub use_agents: Vec<Agent>,
}

#[derive(Debug, Clone, Default)]
pub struct RouterConfig {
    pub default: Option<Agent>,
    pub rules: Vec<RouterRule>,
}

/// Decides which agent should run `task`, or `None` if nothing local fits —
/// the caller's cue to fall back (e.g. cede via the bridge).
///
/// Precedence, most specific first: explicit assignment on the task beats
/// every rule; the first matching rule with an installed preference beats
/// the default; the default only fires if it is itself installed.
/// `installed` (`detect_all()`'s output, by agent id) is the sole authority
/// on availability — a rule or default naming something not installed falls
/// through rather than hard-failing or silently picking whatever the caller
/// didn't ask for.
#[must_use]
pub fn route(task: &TaskContext, config: &RouterConfig, installed: &[Agent]) -> Option<RouteDecision> {
    if let Some(agent) = task.assigned_agent {
        return Some(RouteDecision {
            agent,
            model: None,
            reason: "explicit assignment on task".to_string(),
        });
    }

    for rule in &config.rules {
        if !rule.when.matches(task) {
            continue;
        }
        if let Some(agent) = rule.use_agents.iter().find(|a| installed.contains(a)) {
            return Some(RouteDecision {
                agent: *agent,
                model: None,
                reason: format!(
                    "matched rule (labels={:?}, kind={:?}, size={:?})",
                    rule.when.labels, rule.when.kind, rule.when.size
                ),
            });
        }
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
            },
            use_agents: use_agents.to_vec(),
        }
    }

    #[test]
    fn explicit_assignment_wins_over_everything() {
        let task = TaskContext {
            assigned_agent: Some(Agent::Codex),
            ..Default::default()
        };
        let config = RouterConfig {
            default: Some(Agent::ClaudeCode),
            rules: vec![rule(&[], &[Agent::Opencode])],
        };
        let decision = route(&task, &config, &[Agent::ClaudeCode]).unwrap();
        assert_eq!(decision.agent, Agent::Codex);
        assert_eq!(decision.reason, "explicit assignment on task");
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
        let decision = route(&task, &config, &[Agent::Opencode]).unwrap();
        assert_eq!(decision.agent, Agent::Opencode);
    }

    #[test]
    fn rule_requires_all_its_labels_present_extra_task_labels_ok() {
        let task = TaskContext {
            labels: vec!["security".to_string(), "auth".to_string(), "urgent".to_string()],
            ..Default::default()
        };
        let config = RouterConfig {
            default: None,
            rules: vec![rule(&["security", "auth"], &[Agent::ClaudeCode])],
        };
        assert!(route(&task, &config, &[Agent::ClaudeCode]).is_some());
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
        let decision = route(&task, &config, &[Agent::Opencode]).unwrap();
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
            }],
        };
        assert!(route(&task, &config, &[Agent::ClaudeCode]).is_none());
    }

    #[test]
    fn kind_and_size_must_match_when_specified() {
        let rule = RouterRule {
            when: RuleMatch {
                labels: vec![],
                kind: Some("locate".to_string()),
                size: Some("S".to_string()),
            },
            use_agents: vec![Agent::Opencode],
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
        assert!(route(&matching, &config, &[Agent::Opencode]).is_some());

        let wrong_size = TaskContext {
            kind: Some("locate".to_string()),
            size: Some("L".to_string()),
            ..Default::default()
        };
        assert!(route(&wrong_size, &config, &[Agent::Opencode]).is_none());
    }

    #[test]
    fn default_used_when_no_rule_matches_and_default_installed() {
        let task = TaskContext::default();
        let config = RouterConfig {
            default: Some(Agent::ClaudeCode),
            rules: vec![],
        };
        let decision = route(&task, &config, &[Agent::ClaudeCode]).unwrap();
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
        assert!(route(&task, &config, &[Agent::Opencode]).is_none());
    }

    #[test]
    fn no_rules_no_default_yields_no_decision() {
        let task = TaskContext::default();
        let config = RouterConfig::default();
        assert!(route(&task, &config, &[Agent::ClaudeCode]).is_none());
    }
}
