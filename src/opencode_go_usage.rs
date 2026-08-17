#![allow(dead_code)] // wired into resolve_agent's fallback gate in cli/work.rs

//! Detects whether the active OpenCode Go subscription's usage (rolling
//! 5-hour, weekly, or monthly window) is at/over the fallback threshold, so
//! `resolve_agent`'s router-driven fallback (`src/cli/work.rs`) knows when
//! to prefer another installed agent CLI for the SDD loop's implementer
//! role — the OpenCode Go counterpart to `claude_usage`'s Claude Max
//! 5h/7d gate.
//!
//! OpenCode Go exposes its quota windows via the official
//! `GET https://opencode.ai/zen/go/v1/usage` endpoint, authenticated with
//! the same API key opencode stores locally in its `auth.json` (the
//! `opencode-go` provider). No OAuth refresh, no dashboard scraping. Fails
//! open (treats usage as "under threshold") on any missing key, network, or
//! parse error — a transient hiccup here must never block dispatch.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const USAGE_URL: &str = "https://opencode.ai/zen/go/v1/usage";
const FALLBACK_THRESHOLD_PERCENT: f32 = 70.0;
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Extracts the `opencode-go` API key from opencode's `auth.json` content —
/// the shape is `{"opencode-go": {"type": "api", "key": "sk-..."}}`. Any
/// other shape (missing provider, missing/empty/non-string key, invalid
/// JSON) is treated as "no key" rather than an error, so a file this parser
/// doesn't fully understand still fails open instead of hard-erroring.
fn parse_auth_key(text: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid auth JSON: {e}"))?;
    value
        .get("opencode-go")
        .and_then(|v| v.get("key"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "no opencode-go API key in auth.json".to_string())
}

fn read_api_key() -> Result<String, String> {
    let path = crate::paths::opencode_auth_path();
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    parse_auth_key(&text)
}

/// True when any of the three windows is at/over the fallback threshold.
fn is_over_threshold(rolling_percent: f32, weekly_percent: f32, monthly_percent: f32) -> bool {
    rolling_percent >= FALLBACK_THRESHOLD_PERCENT
        || weekly_percent >= FALLBACK_THRESHOLD_PERCENT
        || monthly_percent >= FALLBACK_THRESHOLD_PERCENT
}

/// Fetches the three usage percentages. Not unit tested directly (no
/// HTTP-mocking dependency in this crate — `claude_usage`'s `fetch_usage`
/// sets the same precedent of leaving the real network call untested and
/// testing only the pure logic around it).
fn fetch_usage_percentages(api_key: &str) -> Result<(f32, f32, f32), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(10))
        .build();
    let response = agent
        .get(USAGE_URL)
        .set("Accept", "application/json")
        .set("User-Agent", "opencode-go/1.0.0 (agentflare)")
        .set("Authorization", &format!("Bearer {api_key}"))
        .call()
        .map_err(|e| format!("usage request failed: {e}"))?;
    let body: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("could not parse usage response: {e}"))?;
    let usage = body
        .get("usage")
        .ok_or_else(|| "usage response missing \"usage\" field".to_string())?;
    // `percent` is an integer on the wire today; parse defensively so a
    // switch to fractional percentages doesn't silently read as 0.
    let pct = |window: &str| -> f32 {
        usage
            .get(window)
            .and_then(|w| w.get("percent"))
            .and_then(|v| v.as_f64().or_else(|| v.as_i64().map(|i| i as f64)))
            .unwrap_or(0.0) as f32
    };
    Ok((pct("rolling"), pct("weekly"), pct("monthly")))
}

struct CacheEntry {
    over_threshold: bool,
    fetched_at: Instant,
}

static CACHE: OnceLock<Mutex<Option<CacheEntry>>> = OnceLock::new();

/// True when the active OpenCode Go account's rolling/weekly/monthly usage
/// is at/over the fallback threshold. Fails open (`false`) on any key-read,
/// network, or parse error. Cached 5 minutes so this never adds a network
/// call per SDD-loop turn.
pub fn opencode_go_over_threshold() -> bool {
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache.lock().unwrap();
        if let Some(entry) = guard.as_ref()
            && entry.fetched_at.elapsed() < CACHE_TTL
        {
            return entry.over_threshold;
        }
    }

    let over_threshold = (|| -> Result<bool, String> {
        let api_key = read_api_key()?;
        let (rolling, weekly, monthly) = fetch_usage_percentages(&api_key)?;
        Ok(is_over_threshold(rolling, weekly, monthly))
    })()
    .unwrap_or(false);

    *cache.lock().unwrap() = Some(CacheEntry {
        over_threshold,
        fetched_at: Instant::now(),
    });
    over_threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_over_threshold_true_when_rolling_at_seventy() {
        assert!(is_over_threshold(70.0, 10.0, 10.0));
    }

    #[test]
    fn is_over_threshold_true_when_weekly_at_seventy() {
        assert!(is_over_threshold(10.0, 70.0, 10.0));
    }

    #[test]
    fn is_over_threshold_true_when_monthly_at_seventy() {
        assert!(is_over_threshold(10.0, 10.0, 70.0));
    }

    #[test]
    fn is_over_threshold_false_when_all_under_seventy() {
        assert!(!is_over_threshold(69.9, 69.9, 69.9));
    }

    #[test]
    fn parse_auth_key_reads_the_opencode_go_entry() {
        let key = parse_auth_key(r#"{"opencode-go":{"type":"api","key":"sk-test"}}"#).unwrap();
        assert_eq!(key, "sk-test");
    }

    #[test]
    fn parse_auth_key_ignores_other_providers_without_opencode_go() {
        let err = parse_auth_key(r#"{"opencode":{"type":"api","key":"sk-other"}}"#).unwrap_err();
        assert!(err.contains("opencode-go"));
    }

    #[test]
    fn parse_auth_key_rejects_a_missing_key_field() {
        let err = parse_auth_key(r#"{"opencode-go":{"type":"api"}}"#).unwrap_err();
        assert!(err.contains("opencode-go"));
    }

    #[test]
    fn parse_auth_key_rejects_an_empty_key() {
        let err = parse_auth_key(r#"{"opencode-go":{"type":"api","key":""}}"#).unwrap_err();
        assert!(err.contains("opencode-go"));
    }

    #[test]
    fn parse_auth_key_rejects_invalid_json() {
        let err = parse_auth_key("not json").unwrap_err();
        assert!(err.contains("invalid auth JSON"));
    }

    #[test]
    fn read_api_key_fails_open_when_file_is_missing() {
        crate::paths::test_support::with_temp_home(|| {
            let err = read_api_key().unwrap_err();
            assert!(err.contains("could not read"));
        });
    }

    #[test]
    fn read_api_key_reads_a_real_file() {
        crate::paths::test_support::with_temp_home(|| {
            let path = crate::paths::opencode_auth_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, r#"{"opencode-go":{"type":"api","key":"sk-test"}}"#).unwrap();
            assert_eq!(read_api_key().unwrap(), "sk-test");
        });
    }

    // The only test exercising the cached wrapper directly — `CACHE` is a
    // process-wide static, so a second test hitting `opencode_go_over_threshold`
    // could observe the first test's cached result depending on `cargo
    // test`'s thread scheduling. Keep it this way, mirroring claude_usage.
    #[test]
    fn opencode_go_over_threshold_fails_open_without_credentials() {
        crate::paths::test_support::with_temp_home(|| {
            assert!(!opencode_go_over_threshold());
        });
    }
}
