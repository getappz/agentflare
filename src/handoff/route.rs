//! Failover routing: when one agent CLI is exhausted (credits, rate
//! limit, auth), suggest — or with `--execute` perform — a handoff to the
//! next eligible agent. Suggestion-only by default: this module never reads
//! credentials and never bypasses quotas; it only moves already-authorized
//! work between CLIs the user authenticated themselves.
//!
//! Eligibility = live sessions first (same host registry the claim
//! liveness sweep uses), then static priority for agents with no live
//! session. The exhausted agent is always excluded: failing over to the
//! same wallet that just ran dry helps nobody. Chain depth caps at 5.

use std::collections::HashSet;

/// Static fallback priority when no live session exists for an agent.
/// Ordered cheapest-general-first: the common trigger is an exhausted
/// paid subscription, so multi-provider CLIs sort ahead of single-vendor
/// ones.
pub const PRIORITY: &[&str] = &["opencode", "codex", "claude_code", "gemini"];
/// Max handoffs in one failover chain (endy parity: prevents runaway loops).
pub const MAX_DEPTH: u32 = 5;
/// Kill-switch: set to refuse automatic selection (explicit `--to` still
/// works — a human already decided).
pub const NO_AUTO_ENV: &str = "AGENTFLARE_NO_AUTO_HANDOFF";

/// Known exhaustion signals, `(needle, canonical name)`. Matched
/// case-insensitively against a log excerpt or error string.
const SIGNALS: &[(&str, &str)] = &[
    ("usage_limit_exceeded", "quota"),
    ("resource_exhausted", "rate_limit"),
    ("providermodelnotfounderror", "model"),
    ("reached maximum conversation turns", "turn_limit"),
    ("model_not_supported", "model"),
    ("credit balance is too low", "quota"),
    ("quota exceeded", "quota"),
    ("rate limited", "rate_limit"),
    ("rate_limit", "rate_limit"),
    ("429", "rate_limit"),
];

/// Canonical exhaustion class for a log excerpt, if any is recognized.
pub fn detect_signal(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    // Multi-word needles carry their own spaces; single tokens match on the
    // spaceless text so `ProviderModelNotFoundError` still hits.
    let flat: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    SIGNALS
        .iter()
        .find(|(needle, _)| {
            if needle.contains(' ') {
                lower.contains(needle)
            } else {
                flat.contains(needle)
            }
        })
        .map(|(_, name)| *name)
}

/// Rank candidates: live agents first (in registry order), then static
/// priority for the rest. The exhausted agent is excluded throughout.
pub fn rank(live: &[String], from: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for agent in live
        .iter()
        .cloned()
        .chain(PRIORITY.iter().map(|s| (*s).to_string()))
    {
        if agent == from || !seen.insert(agent.clone()) {
            continue;
        }
        out.push(agent);
    }
    out
}

/// Live agent names right now (best-effort: registry failures yield an
/// empty list and routing falls back to static priority).
pub fn live_agents() -> Vec<String> {
    let Ok(conn) = crate::db::open() else {
        return Vec::new();
    };
    crate::sessions::list_live(&conn, crate::claims::now())
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.agent)
        .collect::<Vec<_>>()
}

pub struct RouteRequest {
    /// Exhausted agent (`claude_code`, `codex`, ...; aliases accepted).
    pub from: String,
    /// Log excerpt / error text; a recognized signal is reported back.
    pub reason: Option<String>,
    /// Explicit target: skips auto-selection (still honors the depth cap).
    pub to: Option<String>,
    /// Current chain depth; refuses at [`MAX_DEPTH`].
    pub depth: u32,
    /// With `to` (or the recommendation): perform `send` instead of only
    /// suggesting. Needs the foreign session id.
    pub execute: bool,
    pub session_id: Option<String>,
    pub verbosity: String,
}

#[derive(Debug)]
pub struct RouteOutcome {
    pub from: String,
    pub signal: Option<String>,
    pub recommended: Option<String>,
    pub alternatives: Vec<String>,
    pub depth: u32,
    /// Present only when `execute` ran a real send.
    pub sent: Option<super::SendOutcome>,
}

/// Suggest (and optionally execute) the next agent. Pure suggestion unless
/// `execute` is set with a session id.
pub fn route(req: RouteRequest) -> Result<RouteOutcome, String> {
    if req.depth >= MAX_DEPTH {
        return Err(format!(
            "failover chain depth {0} reached (cap {MAX_DEPTH}) — stop and ask a human",
            req.depth
        ));
    }
    let from = super::sources::normalize_source(&req.from).map_err(|_| {
        format!(
            "unknown agent '{}' — use one of: {}",
            req.from,
            super::sources::SUPPORTED.join(", ")
        )
    })?;
    let signal = req
        .reason
        .as_deref()
        .and_then(detect_signal)
        .map(str::to_string);
    let alternatives = rank(&live_agents(), &from);
    let recommended = req.to.clone().or_else(|| alternatives.first().cloned());

    if std::env::var(NO_AUTO_ENV).is_ok_and(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        && req.to.is_none()
    {
        return Err(format!(
            "{NO_AUTO_ENV} is set — automatic selection refused; re-run with an explicit --to"
        ));
    }
    let recommended =
        recommended.ok_or_else(|| format!("no eligible agent to take over from {from}"))?;

    let sent = if req.execute {
        let Some(session_id) = req.session_id.clone() else {
            return Err("--execute needs a session id (--session)".into());
        };
        Some(super::send(super::SendRequest {
            source: from.clone(),
            session_id,
            target: recommended.clone(),
            verbosity: req.verbosity.clone(),
            thread: None,
            reply_to: None,
            name: None,
            artifact_dir: None,
        })?)
    } else {
        None
    };
    Ok(RouteOutcome {
        from,
        signal,
        recommended: Some(recommended),
        alternatives,
        depth: req.depth,
        sent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_classify_known_excerpts() {
        assert_eq!(
            detect_signal("error: usage_limit_exceeded, reset at midnight"),
            Some("quota")
        );
        assert_eq!(
            detect_signal("RESOURCE_EXHAUSTED: quota it"),
            Some("rate_limit")
        );
        assert_eq!(
            detect_signal("ProviderModelNotFoundError: opus"),
            Some("model")
        );
        assert_eq!(detect_signal("all good, 200 OK"), None);
    }

    #[test]
    fn rank_prefers_live_and_drops_exhausted() {
        let ranked = rank(&["gemini".into(), "codex".into()], "codex");
        assert_eq!(ranked.first().map(String::as_str), Some("gemini"));
        assert!(!ranked.contains(&"codex".to_string()));
        // Static priority fills agents with no live session, deduped.
        assert!(ranked.contains(&"opencode".to_string()));
        assert_eq!(ranked.len(), ranked.iter().collect::<HashSet<_>>().len());
    }

    #[test]
    fn depth_cap_refuses() {
        let err = route(RouteRequest {
            from: "codex".into(),
            reason: None,
            to: None,
            depth: MAX_DEPTH,
            execute: false,
            session_id: None,
            verbosity: "minimal".into(),
        })
        .unwrap_err();
        assert!(err.contains("cap"), "{err}");
    }

    #[test]
    fn explicit_to_wins_over_recommendation() {
        let out = route(RouteRequest {
            from: "codex".into(),
            reason: Some("quota exceeded".into()),
            to: Some("opencode".into()),
            depth: 0,
            execute: false,
            session_id: None,
            verbosity: "minimal".into(),
        })
        .unwrap();
        assert_eq!(out.recommended.as_deref(), Some("opencode"));
        assert_eq!(out.signal.as_deref(), Some("quota"));
        assert!(out.sent.is_none());
    }
}
