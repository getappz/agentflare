//! Free-tier quota tracking and provider selection.
//!
//! Design ported from two real-world references (adapted, not copied --
//! different language and a smaller candidate set than either):
//!
//! - **Window get-or-reset-or-increment**: LiteLLM's provider budget limiter
//!   (github.com/BerriAI/litellm, `litellm/router_strategy/budget_limiter.py`,
//!   MIT) tracks a `(spend, window_start)` pair per provider and, on each
//!   request, either starts a fresh window, resets an expired one, or
//!   increments within the current one. `record_consumption` below is that
//!   same three-way branch, without LiteLLM's Redis/multi-instance sync (we
//!   have one proxy process, not a fleet).
//! - **Fail-open, try-in-order fallback**: OmniRoute's emergency fallback
//!   (github.com/diegosouzapw/OmniRoute, `open-sse/services/emergencyFallback.ts`,
//!   MIT) never lets a tracking bug block a request, and walks a short
//!   ordered candidate list rather than re-ranking a large deployment pool.
//!   `select_best` and the fail-open I/O below follow that shape.
//!
//! Persisted usage lives next to the provider registry cache (same
//! `agentflare_home()` convention as `registry.rs`). All I/O here is
//! fail-open: a read/write error never blocks a request, it just means the
//! caller can't skip an exhausted provider this time.
//!
//! Not yet wired into `forward.rs`'s request path -- see item #438 followups.
//! `ProviderConfig`/`ModelRoute` currently resolve to exactly one provider
//! per request; using `select_best` for real routing needs a candidate list
//! there first, and `record_consumption` needs a token count, which today's
//! streaming response path doesn't extract. This module is the tracking/
//! selection primitive that follow-up wires in, tested standalone.

use super::registry::{agentflare_home, RegistrySpec};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const DAY_SECONDS: u64 = 24 * 3600;
const MONTH_SECONDS: u64 = 30 * DAY_SECONDS;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct ProviderUsage {
    /// Unix seconds when the current tracking window started.
    window_start: u64,
    /// Tokens consumed within the current window. For `one-time-initial`
    /// this is lifetime consumption against `credit_tokens` -- that window
    /// never resets (see `window_seconds`).
    tokens_used: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct UsageStore {
    /// Keyed by `RegistrySpec::prefix`.
    providers: HashMap<String, ProviderUsage>,
}

/// How often a provider's tracked window resets. `None` = never resets
/// (`one-time-initial`, a lifetime credit) or isn't tracked at all.
fn window_seconds(free_type: &str) -> Option<u64> {
    match free_type {
        "recurring-daily" => Some(DAY_SECONDS),
        "recurring-monthly" => Some(MONTH_SECONDS),
        _ => None,
    }
}

/// The token budget to track against, if this provider has one worth
/// tracking. `None` means "always available" -- paid/pass-through
/// providers with no free-tier data, `recurring-uncapped` (real access,
/// no published token cap), and `unmetered` (local inference).
fn budget_tokens(spec: &RegistrySpec) -> Option<u64> {
    match spec.free_type.as_deref() {
        Some("one-time-initial") => spec.credit_tokens.filter(|&t| t > 0),
        Some("recurring-daily") | Some("recurring-monthly") => {
            spec.monthly_tokens.filter(|&t| t > 0)
        }
        _ => None,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn store_path() -> PathBuf {
    agentflare_home().join("flare-proxy-usage.json")
}

fn load_store() -> UsageStore {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_store(store: &UsageStore) {
    let path = store_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(store) {
        let _ = std::fs::write(path, json);
    }
}

/// Fraction of budget remaining (0.0 = exhausted, 1.0 = untouched), or
/// `None` for a provider with no tracked budget (see `budget_tokens`) --
/// callers should treat `None` as "always available".
pub fn remaining_fraction(spec: &RegistrySpec) -> Option<f64> {
    let budget = budget_tokens(spec)?;
    let store = load_store();
    let usage = store
        .providers
        .get(&spec.prefix)
        .copied()
        .unwrap_or_default();
    let window = window_seconds(spec.free_type.as_deref().unwrap_or(""));
    let expired = window
        .map(|w| now_secs().saturating_sub(usage.window_start) >= w)
        .unwrap_or(false);
    let used = if expired { 0 } else { usage.tokens_used };
    Some((1.0 - (used as f64 / budget as f64)).max(0.0))
}

/// True if the provider is untracked (always available) or still has
/// budget left this window.
pub fn has_budget(spec: &RegistrySpec) -> bool {
    remaining_fraction(spec).is_none_or(|f| f > 0.0)
}

/// Record token consumption after a successful request. No-op for
/// untracked providers. Fail-open: an I/O error here is silently dropped,
/// same as `registry.rs`'s cache write -- a lost usage record just means
/// the next check reads slightly stale data, not a blocked request.
pub fn record_consumption(spec: &RegistrySpec, tokens: u64) {
    if tokens == 0 || budget_tokens(spec).is_none() {
        return;
    }
    let window = window_seconds(spec.free_type.as_deref().unwrap_or(""));
    let now = now_secs();
    let mut store = load_store();
    let entry = store.providers.entry(spec.prefix.clone()).or_default();
    let expired = window
        .map(|w| now.saturating_sub(entry.window_start) >= w)
        .unwrap_or(false);
    if expired || entry.window_start == 0 {
        entry.window_start = now;
        entry.tokens_used = tokens;
    } else {
        entry.tokens_used += tokens;
    }
    save_store(&store);
}

/// Pick the first candidate, in the given priority order, that still has
/// budget. Untracked candidates always qualify. This walks an ordered
/// fallback chain rather than re-ranking by lowest usage -- flare-proxy's
/// candidate lists are short, deliberately-ordered priority chains (a
/// human picked the order), not a pool of interchangeable deployments to
/// balance load across.
pub fn select_best<'a>(
    candidates: impl IntoIterator<Item = &'a RegistrySpec>,
) -> Option<&'a RegistrySpec> {
    candidates.into_iter().find(|spec| has_budget(spec))
}

#[cfg(test)]
mod tests {
    use super::super::ProviderKind;
    use super::*;

    fn spec(
        prefix: &str,
        free_type: Option<&str>,
        monthly: Option<u64>,
        credit: Option<u64>,
    ) -> RegistrySpec {
        RegistrySpec {
            prefix: prefix.into(),
            id: prefix.into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: Some("https://example.test/v1".into()),
            base_url_template: None,
            api_key_env: None,
            extra_headers: vec![],
            gateway_auth_env: None,
            gateway_auth_header: None,
            free_type: free_type.map(String::from),
            monthly_tokens: monthly,
            credit_tokens: credit,
        }
    }

    fn with_isolated_store<T>(f: impl FnOnce() -> T) -> T {
        // Same lock `registry.rs`'s tests use for AGENTFLARE_HOME_OVERRIDE --
        // it's a process-global env var, so any test in this crate that sets
        // it must serialize against every other one, not just its own module.
        let _guard = crate::test_env_lock();
        let dir =
            std::env::temp_dir().join(format!("flare-proxy-quota-test-{}", std::process::id()));
        std::env::set_var("AGENTFLARE_HOME_OVERRIDE", &dir);
        let _ = std::fs::remove_file(store_path());
        let result = f();
        let _ = std::fs::remove_file(store_path());
        std::env::remove_var("AGENTFLARE_HOME_OVERRIDE");
        result
    }

    #[test]
    fn untracked_provider_always_has_budget() {
        with_isolated_store(|| {
            let anthropic = spec("anthropic", None, None, None);
            let unmetered = spec("lmstudio", Some("unmetered"), None, None);
            let uncapped = spec("siliconflow", Some("recurring-uncapped"), None, None);
            assert_eq!(remaining_fraction(&anthropic), None);
            assert!(has_budget(&anthropic));
            assert!(has_budget(&unmetered));
            assert!(has_budget(&uncapped));
        });
    }

    #[test]
    fn consumption_reduces_remaining_fraction() {
        with_isolated_store(|| {
            let groq = spec("groq", Some("recurring-daily"), Some(1_000_000), None);
            assert_eq!(remaining_fraction(&groq), Some(1.0));
            record_consumption(&groq, 250_000);
            assert_eq!(remaining_fraction(&groq), Some(0.75));
            record_consumption(&groq, 750_000);
            assert_eq!(remaining_fraction(&groq), Some(0.0));
            assert!(!has_budget(&groq));
        });
    }

    #[test]
    fn overconsumption_floors_at_zero_not_negative() {
        with_isolated_store(|| {
            let groq = spec("groq", Some("recurring-daily"), Some(1_000_000), None);
            record_consumption(&groq, 5_000_000);
            assert_eq!(remaining_fraction(&groq), Some(0.0));
        });
    }

    #[test]
    fn expired_window_resets_usage() {
        with_isolated_store(|| {
            let groq = spec("groq", Some("recurring-daily"), Some(1_000_000), None);
            record_consumption(&groq, 1_000_000);
            assert!(!has_budget(&groq));

            // Simulate a day+ having passed by backdating window_start directly
            // in the store, rather than sleeping in a test.
            let mut store = load_store();
            store.providers.get_mut("groq").unwrap().window_start = now_secs() - DAY_SECONDS - 1;
            save_store(&store);

            assert_eq!(remaining_fraction(&groq), Some(1.0));
            assert!(has_budget(&groq));
        });
    }

    #[test]
    fn one_time_credit_never_resets() {
        with_isolated_store(|| {
            let deepseek = spec("deepseek", Some("one-time-initial"), None, Some(1_000_000));
            record_consumption(&deepseek, 1_000_000);
            assert!(!has_budget(&deepseek));

            let mut store = load_store();
            store.providers.get_mut("deepseek").unwrap().window_start =
                now_secs() - MONTH_SECONDS * 12;
            save_store(&store);

            // No window for one-time-initial -- staleness never revives it.
            assert!(!has_budget(&deepseek));
        });
    }

    #[test]
    fn select_best_skips_exhausted_and_keeps_priority_order() {
        with_isolated_store(|| {
            let primary = spec("groq", Some("recurring-daily"), Some(1_000_000), None);
            let secondary = spec("cerebras", Some("recurring-daily"), Some(1_000_000), None);
            let fallback = spec("lmstudio", Some("unmetered"), None, None);
            let candidates = [&primary, &secondary, &fallback];

            assert_eq!(
                select_best(candidates).map(|s| s.prefix.as_str()),
                Some("groq")
            );

            record_consumption(&primary, 1_000_000);
            assert_eq!(
                select_best(candidates).map(|s| s.prefix.as_str()),
                Some("cerebras")
            );

            record_consumption(&secondary, 1_000_000);
            assert_eq!(
                select_best(candidates).map(|s| s.prefix.as_str()),
                Some("lmstudio")
            );
        });
    }

    #[test]
    fn select_best_returns_none_when_all_exhausted() {
        with_isolated_store(|| {
            let only = spec("groq", Some("recurring-daily"), Some(1_000_000), None);
            record_consumption(&only, 1_000_000);
            assert!(select_best([&only]).is_none());
        });
    }
}
