#![allow(dead_code)] // wired into resolve_agent by a later task in the plan

//! Detects whether the active Claude account's subscription usage (5-hour
//! or 7-day window) is at/over the fallback threshold, so
//! `resolve_agent`'s router-driven fallback (`src/cli/work.rs`) knows when
//! to prefer another installed agent CLI for the SDD loop's implementer
//! role. Spec: `leanstack-specs` artifact session,
//! "2026-08-17-claude-usage-fallback".
//!
//! No OAuth refresh: reads whatever access token is currently on disk and
//! fails open (treats usage as "under threshold") on any missing/expired
//! credential, network, or parse error — a transient hiccup here must
//! never block dispatch.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Which usage window breached its configured threshold, and by how much --
/// carried through so the failover log/comment can say what was actually
/// measured instead of an opaque "over its usage threshold".
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct UsageBreach {
    pub window: &'static str,
    pub percent: f32,
    pub threshold: f32,
}

impl std::fmt::Display for UsageBreach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} usage {:.0}% >= threshold {:.0}%",
            self.window, self.percent, self.threshold
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ClaudeCredentials {
    access_token: String,
    expires_at_ms: i64,
}

/// Parses `~/.claude/.credentials.json`'s content. Accepts both the
/// `{"claudeAiOauth": {...}}` wrapper Claude Code writes and a bare
/// `{"accessToken": ..., "expiresAt": ...}` object. `expiresAt` may be a
/// JSON number (epoch ms, the standard shape) or a numeric string; any
/// other shape is treated as already-expired (0) rather than an error, so
/// a credentials file this parser doesn't fully understand still fails
/// open instead of hard-erroring.
fn parse_credentials(text: &str) -> Result<ClaudeCredentials, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid credentials JSON: {e}"))?;
    let oauth = value.get("claudeAiOauth").unwrap_or(&value);
    let access_token = oauth
        .get("accessToken")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing accessToken".to_string())?
        .to_string();
    let expires_at_ms = oauth
        .get("expiresAt")
        .and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
        })
        .unwrap_or(0);
    Ok(ClaudeCredentials {
        access_token,
        expires_at_ms,
    })
}

fn read_credentials_file() -> Result<ClaudeCredentials, String> {
    let path = crate::paths::home()
        .join(".claude")
        .join(".credentials.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    parse_credentials(&text)
}

/// The first usage window (5-hour checked before 7-day) at/over its
/// threshold, or `None` when both have headroom.
fn detect_breach(
    five_hour_percent: f32,
    seven_day_percent: f32,
    five_hour_threshold: f32,
    seven_day_threshold: f32,
) -> Option<UsageBreach> {
    if five_hour_percent >= five_hour_threshold {
        Some(UsageBreach {
            window: "five_hour",
            percent: five_hour_percent,
            threshold: five_hour_threshold,
        })
    } else if seven_day_percent >= seven_day_threshold {
        Some(UsageBreach {
            window: "seven_day",
            percent: seven_day_percent,
            threshold: seven_day_threshold,
        })
    } else {
        None
    }
}

/// Same `anthropic-beta`/`User-Agent` headers Claude Code itself sends —
/// Anthropic authenticates on these; omitting them risks a rejected
/// request. Not unit tested directly (no HTTP-mocking dependency in this
/// crate — see `src/update/github.rs::gh_get` for the same precedent of
/// leaving the real network call untested and testing only the pure logic
/// around it).
fn fetch_usage_percentages(access_token: &str) -> Result<(f32, f32), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(10))
        .build();
    let response = agent
        .get(USAGE_URL)
        .set("Accept", "application/json")
        .set("Content-Type", "application/json")
        .set("User-Agent", "claude-cli/1.0.0 (external, cli)")
        .set("anthropic-beta", "oauth-2025-04-20,claude-code-20250219")
        .set("Authorization", &format!("Bearer {access_token}"))
        .call()
        .map_err(|e| format!("usage request failed: {e}"))?;
    let body: serde_json::Value = response
        .into_json()
        .map_err(|e| format!("could not parse usage response: {e}"))?;
    let pct = |window: &str| -> f32 {
        body.get(window)
            .and_then(|w| w.get("utilization"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0) as f32
    };
    Ok((pct("five_hour"), pct("seven_day")))
}

struct CacheEntry {
    breach: Option<UsageBreach>,
    fetched_at: Instant,
}

static CACHE: OnceLock<Mutex<Option<CacheEntry>>> = OnceLock::new();

/// The usage-window breach (if any) for the active Claude account's 5-hour
/// or 7-day windows, against `~/.agentflare/config.toml`'s `[failover]`
/// thresholds (const default when unset -- see
/// `agent_registry::FailoverConfig`). Fails open (`None`) on any
/// credential-read, expired-token, network, or parse error. Cached 5
/// minutes so this never adds a network call per SDD-loop turn.
pub(crate) fn claude_usage_breach() -> Option<UsageBreach> {
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache.lock().unwrap();
        if let Some(entry) = guard.as_ref()
            && entry.fetched_at.elapsed() < CACHE_TTL
        {
            return entry.breach;
        }
    }

    let breach = (|| -> Result<Option<UsageBreach>, String> {
        let creds = read_credentials_file()?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        if creds.expires_at_ms <= now_ms {
            return Err("access token expired".to_string());
        }
        let (five_hour, seven_day) = fetch_usage_percentages(&creds.access_token)?;
        let config = crate::quota::failover::load_failover_config();
        Ok(detect_breach(
            five_hour,
            seven_day,
            config.five_hour_threshold(),
            config.seven_day_threshold(),
        ))
    })()
    .unwrap_or(None);

    *cache.lock().unwrap() = Some(CacheEntry {
        breach,
        fetched_at: Instant::now(),
    });
    breach
}

/// True when the active Claude account's 5-hour or 7-day usage is at/over
/// the configured fallback threshold. See [`claude_usage_breach`] for the
/// measured-window detail this collapses.
pub fn claude_over_threshold() -> bool {
    claude_usage_breach().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_breach_true_when_five_hour_at_seventy() {
        let breach = detect_breach(70.0, 10.0, 70.0, 70.0).unwrap();
        assert_eq!(breach.window, "five_hour");
        assert_eq!(breach.percent, 70.0);
    }

    #[test]
    fn detect_breach_true_when_seven_day_at_seventy() {
        let breach = detect_breach(10.0, 70.0, 70.0, 70.0).unwrap();
        assert_eq!(breach.window, "seven_day");
        assert_eq!(breach.percent, 70.0);
    }

    #[test]
    fn detect_breach_none_when_both_under_seventy() {
        assert_eq!(detect_breach(69.9, 69.9, 70.0, 70.0), None);
    }

    #[test]
    fn detect_breach_honors_per_window_thresholds() {
        // 74% seven-day trips a 70% threshold but not a 90% one; 10%
        // five-hour never trips either -- this is the item #308 scenario.
        assert_eq!(
            detect_breach(10.0, 74.0, 90.0, 70.0).unwrap().window,
            "seven_day"
        );
        assert_eq!(detect_breach(10.0, 74.0, 90.0, 85.0), None);
    }

    #[test]
    fn breach_display_states_the_measured_window_and_percentage() {
        let breach = UsageBreach {
            window: "seven_day",
            percent: 74.0,
            threshold: 70.0,
        };
        assert_eq!(breach.to_string(), "seven_day usage 74% >= threshold 70%");
    }

    #[test]
    fn parse_credentials_reads_the_wrapped_form() {
        let creds = parse_credentials(
            r#"{"claudeAiOauth": {"accessToken": "tok", "refreshToken": "rt", "expiresAt": 1999999999000}}"#,
        )
        .unwrap();
        assert_eq!(creds.access_token, "tok");
        assert_eq!(creds.expires_at_ms, 1999999999000);
    }

    #[test]
    fn parse_credentials_reads_the_bare_form() {
        let creds = parse_credentials(r#"{"accessToken": "tok2", "expiresAt": 123}"#).unwrap();
        assert_eq!(creds.access_token, "tok2");
        assert_eq!(creds.expires_at_ms, 123);
    }

    #[test]
    fn parse_credentials_reads_a_numeric_string_expires_at() {
        let creds = parse_credentials(r#"{"accessToken": "tok3", "expiresAt": "456"}"#).unwrap();
        assert_eq!(creds.expires_at_ms, 456);
    }

    #[test]
    fn parse_credentials_treats_missing_expires_at_as_expired() {
        let creds = parse_credentials(r#"{"accessToken": "tok4"}"#).unwrap();
        assert_eq!(creds.expires_at_ms, 0);
    }

    #[test]
    fn parse_credentials_rejects_missing_access_token() {
        let err = parse_credentials(r#"{"expiresAt": 123}"#).unwrap_err();
        assert!(err.contains("accessToken"));
    }

    #[test]
    fn parse_credentials_rejects_invalid_json() {
        let err = parse_credentials("not json").unwrap_err();
        assert!(err.contains("invalid credentials JSON"));
    }

    #[test]
    fn read_credentials_file_fails_open_when_file_is_missing() {
        crate::paths::test_support::with_temp_home(|| {
            let err = read_credentials_file().unwrap_err();
            assert!(err.contains("could not read"));
        });
    }

    #[test]
    fn read_credentials_file_reads_a_real_file() {
        crate::paths::test_support::with_temp_home(|| {
            let dir = crate::paths::home().join(".claude");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(".credentials.json"),
                r#"{"claudeAiOauth": {"accessToken": "tok", "expiresAt": 999}}"#,
            )
            .unwrap();
            let creds = read_credentials_file().unwrap();
            assert_eq!(creds.access_token, "tok");
        });
    }

    // The only test exercising the cached wrapper directly — `CACHE` is a
    // process-wide static, so a second test hitting `claude_over_threshold()`
    // could observe the first test's cached result depending on `cargo
    // test`'s thread scheduling. Keep it this way; add new coverage for the
    // cache itself as a pure helper, not a second call through the real
    // static.
    #[test]
    fn claude_over_threshold_fails_open_without_credentials() {
        crate::paths::test_support::with_temp_home(|| {
            assert!(!claude_over_threshold());
        });
    }
}
