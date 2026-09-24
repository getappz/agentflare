//! Per-agent availability and automatic failover for autonomous dispatch.
//!
//! Availability lives in `auth_db`'s existing `cooldowns` table -- the same
//! rows the interactive rotation path (`auth_runner::run`) and the discovery
//! tick's `auth_db::is_cooling_down` gate already use -- so "this agent is
//! out until T" has one source of truth. What this module adds is how long
//! (parsed from the agent's own output, see `auth_runner::AgentFailure`) and
//! who takes over: the next installed, autonomous-capable, currently
//! available agent the operator's config allows.
//!
//! Failover is sticky: the item's `assignee_agent` moves to the new agent and
//! nothing moves it back once the original agent recovers.

use crate::auth_runner::AgentFailure;
use agent_registry::Agent;

/// Cooldown-table profile used when `agent` has no active vault rotation
/// profile -- the common single-credential setup.
pub(crate) const DEFAULT_COOLDOWN_PROFILE: &str = "__default__";

/// `AGENTFLARE_FAILOVER=0|false|off|no` turns automatic failover off
/// (exhausted agents are still cooled down; the item waits for them).
pub(crate) const FAILOVER_ENV: &str = "AGENTFLARE_FAILOVER";

/// Item-metadata key: an array of agent names this item may run on. A
/// failover never picks an agent outside it.
pub(crate) const ALLOWED_AGENTS_KEY: &str = "allowed_agents";
/// Item-metadata key: `"failover": false` pins the item to its assignee.
pub(crate) const FAILOVER_KEY: &str = "failover";

fn env_disabled() -> bool {
    std::env::var(FAILOVER_ENV).is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

/// `~/.agentflare/config.toml`'s `[failover]` table (defaults when absent or
/// unreadable -- a broken config must not stop dispatch).
pub(crate) fn load_failover_config() -> agent_registry::FailoverConfig {
    let path = crate::paths::agentflare_dir().join("config.toml");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| agent_registry::parse_failover_config(&text).ok())
        .unwrap_or_default()
}

/// Whether failover may move this item at all: not disabled by env/config,
/// and not pinned by the item's own `"failover": false`.
pub(crate) fn failover_enabled_for(metadata: &str) -> bool {
    if env_disabled() || !load_failover_config().enabled {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(metadata)
        .ok()
        .and_then(|v| v.get(FAILOVER_KEY).and_then(serde_json::Value::as_bool))
        != Some(false)
}

/// The item's own `allowed_agents` restriction, if it has one.
pub(crate) fn item_allowed_agents(metadata: &str) -> Option<Vec<Agent>> {
    let value: serde_json::Value = serde_json::from_str(metadata).ok()?;
    let list = value.get(ALLOWED_AGENTS_KEY)?.as_array()?;
    Some(
        list.iter()
            .filter_map(serde_json::Value::as_str)
            .filter_map(|s| agent_registry::agent_by_name(&agentflare_backend::item::agent_part(s)))
            .collect(),
    )
}

fn parse_until(until: &str) -> Option<i64> {
    chrono::NaiveDateTime::parse_from_str(until, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|t| t.and_utc().timestamp())
}

/// When `agent` becomes available again (unix seconds) and why, if it is
/// currently cooling down on any profile.
pub(crate) fn unavailable_until(agent: &str) -> Option<(i64, String)> {
    let conn = crate::auth_db::try_open()?;
    crate::auth_db::list_cooldowns(&conn, Some(agent))
        .into_iter()
        .filter_map(|row| Some((parse_until(&row.until)?, row.reason.unwrap_or_default())))
        .max_by_key(|(until, _)| *until)
}

/// Records `agent` as unavailable for `failure`'s wait (see
/// `AgentFailure::wait_secs`) and returns that wait in seconds. Extends,
/// never shortens, an existing cooldown for the same profile. `None` (and
/// nothing recorded) for failures that aren't about availability.
pub(crate) fn mark_unavailable(agent: &str, failure: &AgentFailure, now: i64) -> Option<u64> {
    let secs = failure.wait_secs(now)?;
    let Some(conn) = crate::auth_db::try_open() else {
        return Some(secs);
    };
    let profile = crate::auth_db::get_rotation_last(&conn, agent)
        .map(|(profile, _)| profile)
        .unwrap_or_else(|| DEFAULT_COOLDOWN_PROFILE.to_string());
    let until = now + secs as i64;
    let already_later = crate::auth_db::list_cooldowns(&conn, Some(agent))
        .iter()
        .any(|row| row.profile == profile && parse_until(&row.until).is_some_and(|t| t >= until));
    if !already_later {
        let minutes = secs.div_ceil(60).clamp(1, u64::from(u32::MAX)) as u32;
        crate::auth_db::set_cooldown(&conn, agent, &profile, minutes, failure.describe());
    }
    Some(secs)
}

/// Live usage-threshold check for the subscriptions agentflare can see
/// (claude-code's 5h/7d windows, opencode go's windows); `false` otherwise.
pub(crate) fn over_usage_threshold(agent: Agent) -> bool {
    match agent {
        Agent::ClaudeCode => crate::claude_usage::claude_over_threshold(),
        Agent::Opencode => crate::opencode_go_usage::opencode_go_over_threshold(),
        _ => false,
    }
}

/// Whether `agent` is known to be unable to take work right now.
pub(crate) fn known_unavailable(agent: Agent) -> bool {
    unavailable_until(agent.as_str()).is_some() || over_usage_threshold(agent)
}

/// Pure selection: the first of `candidates` that can run headless and
/// autonomously, is inside `allowed` (when given), and `available` says is
/// up.
pub(crate) fn choose_alternative(
    candidates: &[Agent],
    allowed: Option<&[Agent]>,
    available: impl Fn(Agent) -> bool,
) -> Option<Agent> {
    candidates.iter().copied().find(|agent| {
        agent_registry::autonomous_args(*agent).is_some()
            && agent_registry::headless_args(*agent).is_some()
            && allowed.is_none_or(|list| list.contains(agent))
            && available(*agent)
    })
}

/// The agent `item` should move to now that `exclude` is out, or `None`
/// when failover is off for it or nothing else is available. Candidate
/// order comes from `agent_registry::failover_candidates` (matching
/// `[router]` rules first, then `[failover] agents`, the router default,
/// then any installed agent).
pub(crate) fn find_alternative(
    item: &agentflare_backend::item::Item,
    labels: &[String],
    exclude: Agent,
) -> Option<Agent> {
    if !failover_enabled_for(&item.metadata) {
        return None;
    }
    let mut state = crate::state::load();
    let installed: Vec<Agent> = agent_registry::detect_all_with(
        agent_registry::REGISTRY,
        &mut state.version_cache,
        &agent_registry::RealVersionRunner,
    )
    .iter()
    .filter_map(|d| agent_registry::agent_by_name(d.id))
    .collect();
    crate::state::save(&state);
    let task = agent_registry::TaskContext {
        labels: labels.to_vec(),
        kind: crate::mcp_server::item::parsed_kind(&item.metadata),
        size: crate::mcp_server::item::parsed_size(&item.metadata),
        repo: None,
        assigned_agent: None,
        role: Some("implementer".to_string()),
    };
    let candidates = agent_registry::failover_candidates(
        &task,
        &crate::cli::work::load_router_config(),
        &load_failover_config(),
        &installed,
        exclude,
    );
    let allowed = item_allowed_agents(&item.metadata);
    choose_alternative(&candidates, allowed.as_deref(), |a| !known_unavailable(a))
}

/// Durably moves `item_id` to `to`: sets `assignee_agent` (directly on the
/// backend, not through the MCP reassignment path, which would cancel the
/// very job doing this) and posts the `AGENT_FAILOVER_MARKER` comment that
/// `dispatch_failure_ceiling` treats as neutral. Call only once this job no
/// longer holds the item's claim.
pub(crate) fn record_failover(
    mcp: &crate::mcp_server::AgentflareMcp,
    item_id: &str,
    from: Agent,
    to: Agent,
    reason: &str,
) -> Result<(), String> {
    let author = crate::claims::owner_id();
    mcp.with_backend_db(|conn| -> agentflare_backend::error::Result<()> {
        let tx = conn.unchecked_transaction()?;
        // A `metadata.model` pin names a model of the agent being left
        // (e.g. a Claude model); handing it to the new agent would just fail
        // its launch, so it is dropped (and said so) on the move.
        let item = agentflare_backend::item::get(&tx, item_id)?;
        let mut meta: serde_json::Value =
            serde_json::from_str(&item.metadata).unwrap_or_else(|_| serde_json::json!({}));
        let dropped_model = meta
            .as_object_mut()
            .and_then(|m| m.remove("model"))
            .and_then(|m| m.as_str().map(str::to_string));
        let mut body = format!(
            "{}\n\nmoved from {} to {}: {reason}",
            crate::dispatch_failure_ceiling::AGENT_FAILOVER_MARKER,
            from.as_str(),
            to.as_str()
        );
        if let Some(model) = &dropped_model {
            body.push_str(&format!(
                "\n\nDropped the item's model pin `{model}` (it was for {}).",
                from.as_str()
            ));
        }
        agentflare_backend::item::update(
            &tx,
            item_id,
            agentflare_backend::item::UpdateItem {
                assignee_agent: Some(to.as_str().to_string()),
                metadata: dropped_model.is_some().then(|| meta.to_string()),
                ..Default::default()
            },
        )?;
        agentflare_backend::comment::create(&tx, item_id, &author, &body)?;
        tx.commit()?;
        Ok(())
    })
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Label names on `item_id`, for routing (empty on any lookup failure).
pub(crate) fn item_label_names(
    mcp: &crate::mcp_server::AgentflareMcp,
    item_id: &str,
) -> Vec<String> {
    mcp.with_backend_db(|conn| {
        agentflare_backend::item::list_labels(conn, item_id)
            .unwrap_or_default()
            .iter()
            .filter_map(|id| agentflare_backend::label::get(conn, id).ok())
            .map(|l| l.name)
            .collect::<Vec<_>>()
    })
    .unwrap_or_default()
}

/// Negative cache for the discovery tick: after finding no alternative for
/// an agent, don't re-probe installed agents for it on every tick.
static NO_ALTERNATIVE_UNTIL: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<&'static str, std::time::Instant>>,
> = std::sync::LazyLock::new(Default::default);
const NO_ALTERNATIVE_RECHECK: std::time::Duration = std::time::Duration::from_secs(300);

/// Discovery-time failover (called from `decide::decide_for_supervisor`
/// before an item is dispatched): an item whose assignee is known to be
/// unavailable moves to an available alternative instead of waiting out the
/// cooldown. Returns the agent it moved to.
pub(crate) fn failover_before_dispatch(
    mcp: &crate::mcp_server::AgentflareMcp,
    item: &agentflare_backend::item::Item,
) -> Option<Agent> {
    let agent = item
        .assignee_agent
        .as_deref()
        .and_then(crate::supervisor::resolve_confirmed_agent)?;
    let (until, reason) = unavailable_until(agent.as_str())?;
    if NO_ALTERNATIVE_UNTIL
        .lock()
        .ok()?
        .get(agent.as_str())
        .is_some_and(|t| t.elapsed() < NO_ALTERNATIVE_RECHECK)
    {
        return None;
    }
    let labels = item_label_names(mcp, &item.id);
    let Some(to) = find_alternative(item, &labels, agent) else {
        // The cache is keyed by agent alone, so it may only record "no
        // installed agent is available" -- never a `None` caused by this
        // item's own gates (`failover: false`, `allowed_agents`), which
        // would block failover for every other item of this agent.
        if no_alternative_is_agent_wide(&item.metadata)
            && let Ok(mut cache) = NO_ALTERNATIVE_UNTIL.lock()
        {
            cache.insert(agent.as_str(), std::time::Instant::now());
        }
        return None;
    };
    let why = format!(
        "{} is unavailable ({reason}) until {}",
        agent.as_str(),
        format_unix(until)
    );
    record_failover(mcp, &item.id, agent, to, &why).ok()?;
    Some(to)
}

/// Whether a `None` from [`find_alternative`] for an item with `metadata`
/// says something about the agent rather than the item: failover is on for
/// it and it carries no `allowed_agents` restriction of its own.
fn no_alternative_is_agent_wide(metadata: &str) -> bool {
    failover_enabled_for(metadata) && item_allowed_agents(metadata).is_none()
}

/// `2026-09-24 15:00 UTC` for a unix timestamp.
pub(crate) fn format_unix(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_alternative_skips_unavailable_and_disallowed_agents() {
        let candidates = [Agent::ClaudeCode, Agent::Codex, Agent::Opencode];
        let picked = choose_alternative(&candidates, None, |a| a != Agent::ClaudeCode);
        assert_eq!(picked, Some(Agent::Codex));
        let picked = choose_alternative(&candidates, Some(&[Agent::Opencode]), |_| true);
        assert_eq!(picked, Some(Agent::Opencode));
        assert_eq!(choose_alternative(&candidates, None, |_| false), None);
    }

    #[test]
    fn item_metadata_can_restrict_or_pin() {
        assert_eq!(
            item_allowed_agents(r#"{"allowed_agents":["codex","opencode:abc"]}"#),
            Some(vec![Agent::Codex, Agent::Opencode])
        );
        assert_eq!(item_allowed_agents("{}"), None);
        crate::paths::test_support::with_temp_home(|| {
            assert!(failover_enabled_for("{}"));
            assert!(!failover_enabled_for(r#"{"failover":false}"#));
        });
    }

    #[test]
    fn only_an_unrestricted_item_arms_the_agent_wide_negative_cache() {
        crate::paths::test_support::with_temp_home(|| {
            assert!(no_alternative_is_agent_wide("{}"));
            assert!(!no_alternative_is_agent_wide(r#"{"failover":false}"#));
            assert!(!no_alternative_is_agent_wide(
                r#"{"allowed_agents":["codex"]}"#
            ));
        });
    }

    #[test]
    fn mark_unavailable_records_the_parsed_wait_and_never_shortens_it() {
        crate::paths::test_support::with_temp_home(|| {
            let now = chrono::Utc::now().timestamp();
            let long = AgentFailure::QuotaWindowExhausted {
                resets_at: Some(now + 3 * 3600),
            };
            assert_eq!(mark_unavailable("codex", &long, now), Some(3 * 3600));
            let (until, reason) = unavailable_until("codex").expect("recorded");
            assert!((until - (now + 3 * 3600)).abs() <= 60, "until at the reset");
            assert_eq!(reason, "usage limit reached");
            let short = AgentFailure::RateLimited {
                retry_after_secs: Some(30),
            };
            mark_unavailable("codex", &short, now);
            let (still, _) = unavailable_until("codex").unwrap();
            assert_eq!(still, until, "a shorter wait must not shorten the cooldown");
            assert_eq!(mark_unavailable("codex", &AgentFailure::Other, now), None);
            assert!(unavailable_until("gemini-cli").is_none());
        });
    }
}
