// Which agent takes over a task when the one running it is out of credit or
// quota -- the ordered candidate list; the caller filters it by live
// availability (cooldowns, usage thresholds), which this crate can't see.
use crate::registry::{Agent, REGISTRY, agent_by_name};
use crate::router::{RouterConfig, TaskContext};

/// Default `[failover] usage_threshold_percent` -- the fallback const used
/// when the operator's config sets neither the blanket nor a per-window
/// override. Matches the value every usage-threshold check has hardcoded
/// since item #307/#308's predecessor.
pub const DEFAULT_USAGE_THRESHOLD_PERCENT: f32 = 70.0;

/// `~/.agentflare/config.toml`'s `[failover]` table:
///
/// ```toml
/// [failover]
/// enabled = true                  # default true
/// agents = ["codex", "opencode"]  # allow-list + preference order
/// usage_threshold_percent = 70.0  # default 70.0; blanket usage-window gate
/// five_hour_percent = 90.0        # optional override for Claude's 5h window
/// seven_day_percent = 85.0        # optional override for Claude's 7d window
/// ```
///
/// An empty `agents` list allows any installed agent; a non-empty one is
/// both an allow-list and a preference order.
#[derive(Debug, Clone, PartialEq)]
pub struct FailoverConfig {
    pub enabled: bool,
    pub agents: Vec<Agent>,
    /// Blanket usage-window threshold, applied to every window that has no
    /// more specific override below.
    pub usage_threshold_percent: f32,
    /// Override for Claude's 5-hour window; falls back to
    /// `usage_threshold_percent` when unset.
    pub five_hour_percent: Option<f32>,
    /// Override for Claude's 7-day window; falls back to
    /// `usage_threshold_percent` when unset.
    pub seven_day_percent: Option<f32>,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            agents: Vec::new(),
            usage_threshold_percent: DEFAULT_USAGE_THRESHOLD_PERCENT,
            five_hour_percent: None,
            seven_day_percent: None,
        }
    }
}

impl FailoverConfig {
    /// The effective threshold for Claude's 5-hour window.
    #[must_use]
    pub fn five_hour_threshold(&self) -> f32 {
        self.five_hour_percent.unwrap_or(self.usage_threshold_percent)
    }

    /// The effective threshold for Claude's 7-day window.
    #[must_use]
    pub fn seven_day_threshold(&self) -> f32 {
        self.seven_day_percent.unwrap_or(self.usage_threshold_percent)
    }
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawFailover {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    agents: Vec<String>,
    #[serde(default)]
    usage_threshold_percent: Option<f32>,
    #[serde(default)]
    five_hour_percent: Option<f32>,
    #[serde(default)]
    seven_day_percent: Option<f32>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct RawFile {
    #[serde(default)]
    failover: Option<RawFailover>,
}

/// Parses the `[failover]` table out of a config.toml-shaped document,
/// ignoring every other table. Absent table = defaults. Unknown agent names
/// are dropped, like `parse_router_config` does -- except that a non-empty
/// `agents` list in which *no* name resolves disables failover: an empty
/// list means "any installed agent", so silently dropping every (typo'd)
/// entry would widen an operator's restriction instead of honoring it.
pub fn parse_failover_config(text: &str) -> Result<FailoverConfig, String> {
    let file: RawFile = toml::from_str(text).map_err(|e| e.to_string())?;
    let Some(raw) = file.failover else {
        return Ok(FailoverConfig::default());
    };
    let agents: Vec<Agent> = raw.agents.iter().filter_map(|s| agent_by_name(s)).collect();
    let all_unknown = !raw.agents.is_empty() && agents.is_empty();
    Ok(FailoverConfig {
        enabled: raw.enabled.unwrap_or(true) && !all_unknown,
        agents,
        usage_threshold_percent: raw
            .usage_threshold_percent
            .unwrap_or(DEFAULT_USAGE_THRESHOLD_PERCENT),
        five_hour_percent: raw.five_hour_percent,
        seven_day_percent: raw.seven_day_percent,
    })
}

/// Ordered failover candidates for `task`, excluding `exclude` (the agent
/// that just became unavailable) and anything not in `installed`:
///
/// 1. every `[router]` rule matching the task, in rule order -- the
///    operator's own capability preferences for this kind of work;
/// 2. `failover.agents`, in order;
/// 3. the router default;
/// 4. only when `failover.agents` is empty: every other installed agent,
///    in registry order.
///
/// A non-empty `failover.agents` also restricts every step to that list.
/// `task.assigned_agent` is ignored: it is the pin being failed over from.
#[must_use]
pub fn failover_candidates(
    task: &TaskContext,
    router: &RouterConfig,
    failover: &FailoverConfig,
    installed: &[Agent],
    exclude: Agent,
) -> Vec<Agent> {
    let mut ordered: Vec<Agent> = Vec::new();
    let unpinned = TaskContext {
        assigned_agent: None,
        ..task.clone()
    };
    for rule in &router.rules {
        if rule.when.matches(&unpinned) {
            ordered.extend(rule.use_agents.iter().copied());
        }
    }
    ordered.extend(failover.agents.iter().copied());
    ordered.extend(router.default);
    if failover.agents.is_empty() {
        ordered.extend(REGISTRY.iter().map(|s| s.id));
    }
    let mut out: Vec<Agent> = Vec::new();
    for agent in ordered {
        let allowed = failover.agents.is_empty() || failover.agents.contains(&agent);
        if agent != exclude && allowed && installed.contains(&agent) && !out.contains(&agent) {
            out.push(agent);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::{RouterRule, RuleMatch};

    fn size_rule(size: &str, use_agents: &[Agent]) -> RouterRule {
        RouterRule {
            when: RuleMatch {
                size: Some(size.to_string()),
                ..Default::default()
            },
            use_agents: use_agents.to_vec(),
            rotate: false,
            model: None,
        }
    }

    #[test]
    fn matching_rule_preferences_come_first_then_everything_installed() {
        let task = TaskContext {
            size: Some("large".into()),
            assigned_agent: Some(Agent::ClaudeCode),
            ..Default::default()
        };
        let router = RouterConfig {
            default: None,
            rules: vec![
                size_rule("small", &[Agent::GeminiCli]),
                size_rule("large", &[Agent::ClaudeCode, Agent::Opencode]),
            ],
        };
        let installed = [
            Agent::ClaudeCode,
            Agent::Codex,
            Agent::Opencode,
            Agent::GeminiCli,
        ];
        let got = failover_candidates(
            &task,
            &router,
            &FailoverConfig::default(),
            &installed,
            Agent::ClaudeCode,
        );
        assert_eq!(got[0], Agent::Opencode, "matching rule preference first");
        assert!(
            !got.contains(&Agent::ClaudeCode),
            "the failed agent is excluded"
        );
        assert!(got.contains(&Agent::Codex) && got.contains(&Agent::GeminiCli));
    }

    #[test]
    fn a_failover_list_is_an_allow_list_and_order() {
        let failover = FailoverConfig {
            enabled: true,
            agents: vec![Agent::Opencode, Agent::Codex],
            ..FailoverConfig::default()
        };
        let installed = [
            Agent::ClaudeCode,
            Agent::Codex,
            Agent::Opencode,
            Agent::GeminiCli,
        ];
        let got = failover_candidates(
            &TaskContext::default(),
            &RouterConfig::default(),
            &failover,
            &installed,
            Agent::ClaudeCode,
        );
        assert_eq!(got, vec![Agent::Opencode, Agent::Codex]);
    }

    #[test]
    fn uninstalled_agents_are_never_candidates() {
        let got = failover_candidates(
            &TaskContext::default(),
            &RouterConfig::default(),
            &FailoverConfig::default(),
            &[Agent::ClaudeCode],
            Agent::ClaudeCode,
        );
        assert!(got.is_empty());
    }

    #[test]
    fn parse_reads_the_failover_table() {
        let config = parse_failover_config(
            "[router]\ndefault = \"codex\"\n\n[failover]\nenabled = false\nagents = [\"codex\", \"nope\"]\n",
        )
        .unwrap();
        assert!(!config.enabled);
        assert_eq!(config.agents, vec![Agent::Codex]);
        assert_eq!(
            parse_failover_config("").unwrap(),
            FailoverConfig::default()
        );
    }

    #[test]
    fn usage_threshold_defaults_to_the_const_when_config_omits_it() {
        let config = parse_failover_config("[failover]\nenabled = true\n").unwrap();
        assert_eq!(config.usage_threshold_percent, DEFAULT_USAGE_THRESHOLD_PERCENT);
        assert_eq!(config.five_hour_threshold(), DEFAULT_USAGE_THRESHOLD_PERCENT);
        assert_eq!(config.seven_day_threshold(), DEFAULT_USAGE_THRESHOLD_PERCENT);
        assert_eq!(
            FailoverConfig::default().usage_threshold_percent,
            DEFAULT_USAGE_THRESHOLD_PERCENT
        );
    }

    #[test]
    fn per_window_thresholds_override_the_blanket_percentage() {
        let config = parse_failover_config(
            "[failover]\nusage_threshold_percent = 70.0\nfive_hour_percent = 90.0\nseven_day_percent = 85.0\n",
        )
        .unwrap();
        assert_eq!(config.usage_threshold_percent, 70.0);
        assert_eq!(config.five_hour_threshold(), 90.0);
        assert_eq!(config.seven_day_threshold(), 85.0);
    }

    #[test]
    fn an_allow_list_of_only_unknown_names_disables_failover() {
        let config = parse_failover_config("[failover]\nagents = [\"open-code\"]\n").unwrap();
        assert!(
            !config.enabled,
            "typo-only allow-list must not widen to any agent"
        );
        assert!(config.agents.is_empty());
        let empty = parse_failover_config("[failover]\nagents = []\n").unwrap();
        assert!(empty.enabled);
    }
}
