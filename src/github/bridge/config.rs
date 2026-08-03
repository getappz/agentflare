//! Bridge configuration. Environment-driven for v1, matching current
//! practice; item #331 (unified `~/.agentflare/config.toml`) should absorb
//! these later.

#[allow(dead_code)]
pub const CLAIMED_LABEL_PREFIX: &str = "claimed:";

const DEFAULT_INTERVAL_SECS: u64 = 60;
/// Floor so a mistyped interval cannot turn the loop into a hot GitHub poll.
pub const MIN_INTERVAL_SECS: u64 = 15;
const DEFAULT_MAX_CLAIMS: usize = 3;
const DEFAULT_QUEUE_LABEL: &str = "agentflare";

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct BridgeConfig {
    pub enabled: bool,
    pub interval_secs: u64,
    pub max_claims: usize,
    pub ttl_secs: i64,
    pub queue_label: String,
    pub instance_id: String,
}

fn truthy(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes")
}

#[allow(dead_code)]
impl BridgeConfig {
    pub fn from_env() -> BridgeConfig {
        let get = |k: &str| std::env::var(k).ok();
        let instance = get("AGENTFLARE_BRIDGE_INSTANCE_ID")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(crate::claims::owner_id);
        BridgeConfig::from_values(
            get("AGENTFLARE_BRIDGE_ENABLED").as_deref(),
            get("AGENTFLARE_BRIDGE_INTERVAL_SECS").as_deref(),
            get("AGENTFLARE_BRIDGE_MAX_CLAIMS").as_deref(),
            get("AGENTFLARE_BRIDGE_QUEUE_LABEL").as_deref(),
            instance,
        )
    }

    /// Split out from `from_env` so the parsing rules are testable without
    /// mutating process-global environment state.
    pub fn from_values(
        enabled: Option<&str>,
        interval: Option<&str>,
        max_claims: Option<&str>,
        queue_label: Option<&str>,
        instance_id: String,
    ) -> BridgeConfig {
        BridgeConfig {
            enabled: enabled.is_some_and(truthy),
            interval_secs: interval
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(DEFAULT_INTERVAL_SECS)
                .max(MIN_INTERVAL_SECS),
            max_claims: max_claims
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(DEFAULT_MAX_CLAIMS),
            // Reuses the EXISTING claim TTL so marker liveness and the local
            // ledger expire on one schedule.
            ttl_secs: crate::claims::ttl_secs(),
            queue_label: queue_label
                .filter(|s| !s.is_empty())
                .unwrap_or(DEFAULT_QUEUE_LABEL)
                .to_string(),
            instance_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_off_and_conservative() {
        let c = BridgeConfig::from_values(None, None, None, None, "agent:1".to_string());
        assert!(!c.enabled, "bridge must be opt-in");
        assert_eq!(c.interval_secs, 60);
        assert_eq!(c.max_claims, 3);
        assert_eq!(c.queue_label, "agentflare");
        assert_eq!(c.instance_id, "agent:1");
    }

    #[test]
    fn values_parse_from_strings() {
        let c = BridgeConfig::from_values(
            Some("1"),
            Some("15"),
            Some("7"),
            Some("queue"),
            "agent:1".to_string(),
        );
        assert!(c.enabled);
        assert_eq!(c.interval_secs, 15);
        assert_eq!(c.max_claims, 7);
        assert_eq!(c.queue_label, "queue");
    }

    #[test]
    fn enabled_accepts_common_truthy_spellings() {
        for v in ["1", "true", "TRUE", "yes"] {
            let c = BridgeConfig::from_values(Some(v), None, None, None, "a".to_string());
            assert!(c.enabled, "{v} should enable");
        }
        for v in ["0", "false", "no", "", "banana"] {
            let c = BridgeConfig::from_values(Some(v), None, None, None, "a".to_string());
            assert!(!c.enabled, "{v} should not enable");
        }
    }

    #[test]
    fn garbage_numbers_fall_back_to_defaults_rather_than_panicking() {
        let c = BridgeConfig::from_values(
            Some("1"),
            Some("not-a-number"),
            Some(""),
            None,
            "a".to_string(),
        );
        assert_eq!(c.interval_secs, 60);
        assert_eq!(c.max_claims, 3);
    }

    #[test]
    fn interval_has_a_floor_so_a_typo_cannot_hammer_github() {
        let c = BridgeConfig::from_values(Some("1"), Some("0"), None, None, "a".to_string());
        assert_eq!(c.interval_secs, MIN_INTERVAL_SECS);
    }
}
