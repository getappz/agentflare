// Structured classification of an agent CLI's failure text -- included from
// `auth_runner.rs`. `is_rate_limited`/`is_auth_expired` above only answer
// "retry later?"/"re-authenticate?"; the autonomous dispatch path
// (`cli::work`) also needs to know *how long* an agent is out and whether the
// cause is one another agent can route around (credit exhausted, a usage
// window used up) or a short blip worth retrying on the same agent.

/// A rate limit whose known wait is at or below this keeps retrying the
/// same agent; anything longer (or an exhausted credit/quota) moves the work
/// to another available agent instead.
pub(crate) const SHORT_RATE_LIMIT_SECS: u64 = 120;
/// Wait assumed for a rate limit that printed no retry hint -- the same 30
/// minutes the interactive rotation path (`run` above) has always used.
pub(crate) const DEFAULT_RATE_LIMIT_SECS: u64 = 30 * 60;
/// Wait assumed for a transient overload (`overloaded_error`, 529, a bare
/// "try again") with no retry hint.
const TRANSIENT_RETRY_SECS: u64 = 60;
/// How long an agent that ran out of prepaid credit stays unavailable: a
/// top-up needs a human, so there is no reset time to parse; this is only
/// how often the daemon is willing to find out again.
pub(crate) const CREDIT_EXHAUSTED_SECS: u64 = 6 * 60 * 60;
/// Wait assumed for a usage window that printed no parseable reset time --
/// Claude's rolling window is five hours.
pub(crate) const QUOTA_WINDOW_DEFAULT_SECS: u64 = 5 * 60 * 60;
/// Upper bound on any parsed wait: a garbled reset time must not park an
/// agent for months.
const MAX_UNAVAILABLE_SECS: u64 = 8 * 24 * 60 * 60;

/// Why an agent CLI run failed, as far as its own output says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentFailure {
    /// Throttled; `retry_after_secs` when the agent printed a wait.
    RateLimited {
        retry_after_secs: Option<u64>,
    },
    /// Out of prepaid credit / billing blocked (Claude "Credit balance is
    /// too low", OpenAI `insufficient_quota`, HTTP 402, a spend limit).
    CreditExhausted,
    /// A usage window (5-hour, daily, weekly) is used up; `resets_at` (unix
    /// seconds) when the agent printed when it resets.
    QuotaWindowExhausted {
        resets_at: Option<i64>,
    },
    /// The credential itself is dead -- see `AUTH_EXPIRED_PATTERNS`.
    AuthExpired,
    Other,
}

impl AgentFailure {
    /// Seconds from `now` (unix seconds) until the agent is worth trying
    /// again, or `None` for failures that aren't about availability.
    pub(crate) fn wait_secs(&self, now: i64) -> Option<u64> {
        let secs = match *self {
            AgentFailure::RateLimited { retry_after_secs } => {
                retry_after_secs.unwrap_or(DEFAULT_RATE_LIMIT_SECS)
            }
            AgentFailure::CreditExhausted => CREDIT_EXHAUSTED_SECS,
            AgentFailure::QuotaWindowExhausted { resets_at } => resets_at
                .map(|t| (t - now).max(60) as u64)
                .unwrap_or(QUOTA_WINDOW_DEFAULT_SECS),
            AgentFailure::AuthExpired | AgentFailure::Other => return None,
        };
        Some(secs.clamp(1, MAX_UNAVAILABLE_SECS))
    }

    /// Whether the work should move to another agent rather than wait for
    /// this one: an exhausted credit/quota, or a rate limit longer than
    /// `SHORT_RATE_LIMIT_SECS`.
    pub(crate) fn warrants_failover(&self, now: i64) -> bool {
        match self {
            AgentFailure::CreditExhausted | AgentFailure::QuotaWindowExhausted { .. } => true,
            AgentFailure::RateLimited { .. } => {
                self.wait_secs(now).unwrap_or(0) > SHORT_RATE_LIMIT_SECS
            }
            AgentFailure::AuthExpired | AgentFailure::Other => false,
        }
    }

    /// Short human label, used in comments and the cooldown `reason`.
    pub(crate) fn describe(&self) -> &'static str {
        match self {
            AgentFailure::RateLimited { .. } => "rate limited",
            AgentFailure::CreditExhausted => "out of credit",
            AgentFailure::QuotaWindowExhausted { .. } => "usage limit reached",
            AgentFailure::AuthExpired => "authentication expired",
            AgentFailure::Other => "failed",
        }
    }
}

/// Prepaid-credit / billing exhaustion across Claude Code, Codex/OpenAI,
/// Gemini, OpenCode and Cursor wordings. Checked before the usage-window
/// list: "spend limit" also appears in Cursor's usage-limit message.
const CREDIT_PATTERNS: &[&str] = &[
    "credit balance is too low",
    "credit balance too low",
    "insufficient credit",
    "insufficient_credit",
    "insufficient balance",
    "insufficient_balance",
    "insufficient_quota",
    "out of credits",
    "out of credit",
    "no credits remaining",
    "credits exhausted",
    "credits have been exhausted",
    "ran out of credits",
    "run out of credits",
    "purchase more credits",
    "add more credits",
    "payment required",
    "billing limit",
    "billing hard limit",
    "spend limit",
    "spending limit",
];

/// A usage window used up (5-hour, daily, weekly, monthly plan limits).
const QUOTA_WINDOW_PATTERNS: &[&str] = &[
    "usage limit",
    "hit your limit",
    "you've hit your",
    "you\u{2019}ve hit your",
    "you have hit your",
    "5-hour limit",
    "weekly limit",
    "daily limit",
    "monthly limit",
    "limit will reset",
    "quota exceeded",
    "quota exhausted",
    "exceeded your current quota",
    "exceeded your quota",
    "resource_exhausted",
    "resource exhausted",
];

/// Throttling proper. Bare "429" stays (it has always matched here).
const THROTTLE_PATTERNS: &[&str] = &[
    "429",
    "rate limit",
    "rate_limit",
    "rate-limit",
    "ratelimit",
    "too many requests",
];

/// Transient server-side trouble worth a quick same-agent retry.
const TRANSIENT_PATTERNS: &[&str] = &["overloaded", "temporarily unavailable", "try again"];

/// Classifies `text` (a failure message, possibly carrying the agent's own
/// output tail) against the current time.
pub(crate) fn classify_failure(text: &str) -> AgentFailure {
    classify_failure_at(text, chrono::Utc::now())
}

/// [`classify_failure`] with an explicit `now`, for deterministic tests of
/// reset-time parsing.
pub(crate) fn classify_failure_at(text: &str, now: chrono::DateTime<chrono::Utc>) -> AgentFailure {
    let lower = text.to_lowercase();
    let now_ts = now.timestamp();
    let hint = retry_hint_secs(&lower);
    let has = |patterns: &[&str]| patterns.iter().any(|p| lower.contains(p));

    let classified = if has(CREDIT_PATTERNS) || has_status_code(&lower, "402") {
        AgentFailure::CreditExhausted
    } else if has(QUOTA_WINDOW_PATTERNS) {
        AgentFailure::QuotaWindowExhausted {
            resets_at: parse_reset_at(&lower, now, hint),
        }
    } else if is_auth_expired(text) {
        AgentFailure::AuthExpired
    } else if has(THROTTLE_PATTERNS) {
        AgentFailure::RateLimited {
            retry_after_secs: hint,
        }
    } else if has(TRANSIENT_PATTERNS) || has_status_code(&lower, "529") {
        AgentFailure::RateLimited {
            retry_after_secs: Some(hint.unwrap_or(TRANSIENT_RETRY_SECS)),
        }
    } else {
        AgentFailure::Other
    };

    // A "quota" that the agent itself says clears within the short window
    // (Gemini's per-minute RESOURCE_EXHAUSTED: "Please retry in 26s") is
    // throttling in practice -- retry the same agent, don't move the work.
    match classified {
        AgentFailure::CreditExhausted | AgentFailure::QuotaWindowExhausted { .. }
            if hint.is_some_and(|h| h <= SHORT_RATE_LIMIT_SECS) =>
        {
            AgentFailure::RateLimited {
                retry_after_secs: hint,
            }
        }
        AgentFailure::QuotaWindowExhausted { .. }
            if classified
                .wait_secs(now_ts)
                .is_some_and(|w| w <= SHORT_RATE_LIMIT_SECS) =>
        {
            AgentFailure::RateLimited {
                retry_after_secs: classified.wait_secs(now_ts),
            }
        }
        other => other,
    }
}

/// Whether a failed agent turn should skip the workflow step's own retry
/// policy: retrying an expired credential, an exhausted credit/quota, or a
/// long rate limit seconds later just fails the same way and delays the
/// failover in `cli::work`.
pub(crate) fn skips_step_retry(text: &str) -> bool {
    let failure = classify_failure(text);
    // A hung-but-chatty agent killed by the stall detector would just loop
    // the same way for another stall window on a step retry.
    text.contains(crate::agent_launch::STALLED_MARKER)
        || is_auth_expired(text)
        || matches!(
            failure,
            AgentFailure::AuthExpired
                | AgentFailure::CreditExhausted
                | AgentFailure::QuotaWindowExhausted { .. }
        )
        || (matches!(failure, AgentFailure::RateLimited { .. })
            && failure.warrants_failover(chrono::Utc::now().timestamp()))
}

/// `code` appearing as an HTTP status rather than as any number in passing
/// (a line number, an item id): preceded by "http"/"status"/"error"/"code",
/// or followed by its reason phrase.
fn has_status_code(lower: &str, code: &str) -> bool {
    lower.match_indices(code).any(|(idx, _)| {
        let bytes = lower.as_bytes();
        let end = idx + code.len();
        let digit_before = idx > 0 && bytes[idx - 1].is_ascii_digit();
        let digit_after = end < bytes.len() && bytes[end].is_ascii_digit();
        if digit_before || digit_after {
            return false;
        }
        let mut start = idx.saturating_sub(14);
        while !lower.is_char_boundary(start) {
            start -= 1;
        }
        let before = &lower[start..idx];
        let after = lower[end..].trim_start_matches([' ', ':', '-', '"']);
        ["http", "status", "error", "code"]
            .iter()
            .any(|w| before.contains(w))
            || ["payment", "too many", "overloaded"]
                .iter()
                .any(|w| after.starts_with(w))
    })
}

/// Phrases after which agents print how long to wait.
const RETRY_HINT_KEYWORDS: &[&str] = &[
    "retry after",
    "retry-after",
    "retry_after",
    "retrydelay",
    "retry in",
    "try again in",
    "try again after",
    "resets in",
    "reset in",
    "available again in",
];

/// The first parseable wait after any `RETRY_HINT_KEYWORDS`, in seconds.
fn retry_hint_secs(lower: &str) -> Option<u64> {
    RETRY_HINT_KEYWORDS
        .iter()
        .flat_map(|kw| lower.match_indices(kw).map(move |(i, _)| i + kw.len()))
        .filter_map(|at| parse_duration(&lower[at..]))
        .next()
}

/// Parses a leading duration such as `30`, `26.7s`, `1m30s`, `4 days 3
/// hours 12 minutes`, `2h, 5m and 3s`. A bare number means seconds (the
/// HTTP `Retry-After` form). `None` when no number leads the text.
fn parse_duration(text: &str) -> Option<u64> {
    let s = text.trim_start_matches([' ', ':', '"', '\'', '=']);
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut total = 0f64;
    let mut any = false;
    loop {
        let num_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        if num_start == i {
            break;
        }
        let Ok(num) = s[num_start..i].trim_end_matches('.').parse::<f64>() else {
            break;
        };
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
        }
        let unit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let mult = match &s[unit_start..i] {
            "ms" | "millisecond" | "milliseconds" => 0.001,
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
            "d" | "day" | "days" => 86_400.0,
            "w" | "week" | "weeks" => 604_800.0,
            "" if !any => 1.0,
            _ if !any => return None,
            _ => break,
        };
        total += num * mult;
        any = true;
        // Separators between components: spaces, commas, "and".
        while i < bytes.len() && matches!(bytes[i], b' ' | b',') {
            i += 1;
        }
        if s[i..].starts_with("and ") {
            i += 4;
        }
        if !(i < bytes.len() && bytes[i].is_ascii_digit()) {
            break;
        }
    }
    any.then(|| total.ceil().max(1.0) as u64)
}

static ISO_RESET: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(\d{4}-\d{2}-\d{2})[t ](\d{2}:\d{2}(?::\d{2})?)(?:\.\d+)?(z|[+-]\d{2}:?\d{2})?",
    )
    .expect("valid regex")
});

static CLOCK_RESET: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(
        r"(?:resets?|try again at|available at|until)\s+(?:at\s+)?(\d{1,2})(?::(\d{2}))?\s*(am|pm)?(?:\s*\(([^)]*)\))?",
    )
    .expect("valid regex")
});

static UNIX_RESET: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"\|(\d{10})\b").expect("valid regex"));

/// When a usage window resets, as unix seconds: Claude's legacy
/// `usage limit reached|<unix-ts>`, an ISO-8601 timestamp, a relative
/// `hint` ("try again in 4 days 3 hours"), or a clock time ("resets 3pm
/// (Europe/London)", "try again at 8:05 PM"). Named time zones other than
/// UTC/GMT are read as this machine's local zone -- the zone the agent CLI
/// itself printed them in on the same machine. Results outside
/// `(now, now + MAX_UNAVAILABLE_SECS]` are discarded as misparses.
fn parse_reset_at(
    lower: &str,
    now: chrono::DateTime<chrono::Utc>,
    hint: Option<u64>,
) -> Option<i64> {
    let now_ts = now.timestamp();
    let plausible = |t: i64| t > now_ts && t <= now_ts + MAX_UNAVAILABLE_SECS as i64;

    if let Some(t) = UNIX_RESET
        .captures_iter(lower)
        .filter_map(|c| c[1].parse::<i64>().ok())
        .find(|t| plausible(*t))
    {
        return Some(t);
    }
    for caps in ISO_RESET.captures_iter(lower) {
        let tz = caps.get(3).map_or("z", |m| m.as_str());
        let tz = if tz == "z" {
            "+00:00".to_string()
        } else {
            tz.to_string()
        };
        let time = &caps[2];
        let time = if time.len() == 5 {
            format!("{time}:00")
        } else {
            time.to_string()
        };
        let stamp = format!("{}T{time}{tz}", &caps[1]);
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&stamp)
            .or_else(|_| chrono::DateTime::parse_from_str(&stamp, "%Y-%m-%dT%H:%M:%S%z"))
            && plausible(dt.timestamp())
        {
            return Some(dt.timestamp());
        }
    }
    if let Some(secs) = hint {
        let t = now_ts + secs as i64;
        if plausible(t) {
            return Some(t);
        }
    }
    for caps in CLOCK_RESET.captures_iter(lower) {
        let minute = caps.get(2).map(|m| m.as_str());
        let meridiem = caps.get(3).map(|m| m.as_str());
        // A bare number ("resets 5 hours ...") is not a clock time.
        if minute.is_none() && meridiem.is_none() {
            continue;
        }
        let Ok(mut hour) = caps[1].parse::<u32>() else {
            continue;
        };
        let minute = minute.and_then(|m| m.parse::<u32>().ok()).unwrap_or(0);
        match meridiem {
            Some("pm") if hour < 12 => hour += 12,
            Some("am") if hour == 12 => hour = 0,
            _ => {}
        }
        let Some(clock) = chrono::NaiveTime::from_hms_opt(hour, minute, 0) else {
            continue;
        };
        let utc = caps
            .get(4)
            .is_some_and(|z| matches!(z.as_str().trim(), "utc" | "gmt" | "etc/utc"));
        let next = if utc {
            next_clock_occurrence(now, clock, &chrono::Utc)
        } else {
            next_clock_occurrence(now, clock, &chrono::Local)
        };
        if let Some(t) = next.filter(|t| plausible(*t)) {
            return Some(t);
        }
    }
    None
}

/// The first instant strictly after `now` whose wall-clock time in `tz` is
/// `clock`, as unix seconds.
fn next_clock_occurrence<Tz: chrono::TimeZone>(
    now: chrono::DateTime<chrono::Utc>,
    clock: chrono::NaiveTime,
    tz: &Tz,
) -> Option<i64> {
    let local_now = now.with_timezone(tz);
    let today = local_now.date_naive();
    [today, today.succ_opt()?]
        .into_iter()
        .filter_map(|day| {
            day.and_time(clock)
                .and_local_timezone(tz.clone())
                .earliest()
        })
        .map(|dt| dt.timestamp())
        .find(|t| *t > now.timestamp())
}

#[cfg(test)]
mod classify_tests {
    use super::*;

    fn at() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-09-24T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn classify(text: &str) -> AgentFailure {
        classify_failure_at(text, at())
    }

    #[test]
    fn credit_exhaustion_phrasings_across_agents() {
        for text in [
            // Claude Code (API key / console billing)
            "Credit balance is too low",
            r#"{"type":"result","subtype":"success","is_error":true,"result":"Credit balance is too low"}"#,
            // Anthropic API error body
            "Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits.",
            // OpenAI / Codex
            r#"ERROR: {"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","code":"insufficient_quota"}}"#,
            // OpenRouter / OpenCode providers
            "AI_APICallError: Insufficient credits. Add more using https://openrouter.ai/settings/credits",
            "Error: 402 Payment Required",
            r#"{"status":402,"message":"payment required"}"#,
            "You are out of credits.",
            // Cursor
            "Please set a Spend Limit to continue.",
        ] {
            assert_eq!(classify(text), AgentFailure::CreditExhausted, "{text}");
        }
    }

    #[test]
    fn a_402_that_is_not_a_status_code_is_not_credit_exhaustion() {
        assert_eq!(
            classify("fixed item #402 and line 4021"),
            AgentFailure::Other
        );
    }

    #[test]
    fn quota_window_phrasings_across_agents() {
        for text in [
            "Claude AI usage limit reached",
            "5-hour limit reached \u{2219} resets 3pm",
            "You've hit your limit \u{00b7} resets 3pm (Europe/London)",
            "You've hit your weekly limit",
            "ActionRequiredError: You've hit your usage limit You've saved $51 on API model usage this month with Start. Switch to a different model.",
            "You've hit your usage limit. Upgrade to Pro (https://openai.com/chatgpt/pricing) or try again in 4 days 3 hours 12 minutes.",
            "Quota exceeded for quota metric 'Gemini 2.5 Pro Requests' and limit 'Gemini 2.5 Pro Requests per day'",
            "RESOURCE_EXHAUSTED: daily limit",
        ] {
            assert!(
                matches!(classify(text), AgentFailure::QuotaWindowExhausted { .. }),
                "{text} -> {:?}",
                classify(text)
            );
        }
    }

    #[test]
    fn quota_reset_time_is_parsed_when_printed() {
        let now = at().timestamp();
        // Legacy Claude format: unix timestamp after a pipe.
        let ts = now + 3 * 3600;
        assert_eq!(
            classify(&format!("Claude AI usage limit reached|{ts}")),
            AgentFailure::QuotaWindowExhausted {
                resets_at: Some(ts)
            }
        );
        // Codex relative form.
        assert_eq!(
            classify("You've hit your usage limit. Try again in 4 days 3 hours 12 minutes."),
            AgentFailure::QuotaWindowExhausted {
                resets_at: Some(now + 4 * 86_400 + 3 * 3600 + 12 * 60)
            }
        );
        // ISO timestamp.
        assert_eq!(
            classify("usage limit reached; resets at 2026-09-24T18:30:00Z"),
            AgentFailure::QuotaWindowExhausted {
                resets_at: Some(now + 6 * 3600 + 30 * 60)
            }
        );
        // Clock time in UTC: 3pm today is 3h after noon.
        assert_eq!(
            classify("You've hit your limit \u{00b7} resets 3pm (UTC)"),
            AgentFailure::QuotaWindowExhausted {
                resets_at: Some(now + 3 * 3600)
            }
        );
        // Clock time already past today rolls to tomorrow.
        assert_eq!(
            classify("weekly limit reached, resets at 9:15 am (utc)"),
            AgentFailure::QuotaWindowExhausted {
                resets_at: Some(now + 21 * 3600 + 15 * 60)
            }
        );
        // No reset printed.
        assert_eq!(
            classify("You've hit your weekly limit"),
            AgentFailure::QuotaWindowExhausted { resets_at: None }
        );
    }

    #[test]
    fn rate_limits_parse_retry_after() {
        assert_eq!(
            classify("HTTP 429 Too Many Requests"),
            AgentFailure::RateLimited {
                retry_after_secs: None
            }
        );
        assert_eq!(
            classify("429 Too Many Requests; Retry-After: 30"),
            AgentFailure::RateLimited {
                retry_after_secs: Some(30)
            }
        );
        assert_eq!(
            classify("Rate limit exceeded, please try again in 1m30s"),
            AgentFailure::RateLimited {
                retry_after_secs: Some(90)
            }
        );
        assert_eq!(
            classify("rate_limit_error: retry after 10 minutes"),
            AgentFailure::RateLimited {
                retry_after_secs: Some(600)
            }
        );
    }

    #[test]
    fn gemini_per_minute_quota_with_short_retry_is_a_short_rate_limit() {
        let text = "429 RESOURCE_EXHAUSTED: You exceeded your current quota, please check your plan and billing details. Please retry in 26.738293s.";
        assert_eq!(
            classify(text),
            AgentFailure::RateLimited {
                retry_after_secs: Some(27)
            }
        );
        assert!(!classify(text).warrants_failover(at().timestamp()));
    }

    #[test]
    fn overloaded_is_transient() {
        for text in [
            r#"API Error: 529 {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            "Service temporarily unavailable",
            "something broke, please try again",
        ] {
            let got = classify(text);
            assert_eq!(
                got,
                AgentFailure::RateLimited {
                    retry_after_secs: Some(TRANSIENT_RETRY_SECS)
                },
                "{text}"
            );
            assert!(!got.warrants_failover(at().timestamp()), "{text}");
        }
    }

    #[test]
    fn auth_expired_and_other() {
        assert_eq!(
            classify("Error: session expired, please re-authenticate"),
            AgentFailure::AuthExpired
        );
        assert_eq!(classify("401 Unauthorized"), AgentFailure::AuthExpired);
        assert_eq!(classify("something went wrong"), AgentFailure::Other);
        assert_eq!(
            classify("judge reply was not valid JSON"),
            AgentFailure::Other
        );
    }

    #[test]
    fn failover_policy_by_class() {
        let now = at().timestamp();
        assert!(AgentFailure::CreditExhausted.warrants_failover(now));
        assert!(AgentFailure::QuotaWindowExhausted { resets_at: None }.warrants_failover(now));
        assert!(
            AgentFailure::RateLimited {
                retry_after_secs: None
            }
            .warrants_failover(now),
            "an unhinted rate limit waits DEFAULT_RATE_LIMIT_SECS -- long"
        );
        assert!(
            !AgentFailure::RateLimited {
                retry_after_secs: Some(SHORT_RATE_LIMIT_SECS)
            }
            .warrants_failover(now)
        );
        assert!(!AgentFailure::AuthExpired.warrants_failover(now));
        assert!(!AgentFailure::Other.warrants_failover(now));
        assert_eq!(
            AgentFailure::QuotaWindowExhausted {
                resets_at: Some(now + 7200)
            }
            .wait_secs(now),
            Some(7200)
        );
        assert_eq!(
            AgentFailure::CreditExhausted.wait_secs(now),
            Some(CREDIT_EXHAUSTED_SECS)
        );
        assert_eq!(AgentFailure::Other.wait_secs(now), None);
    }

    #[test]
    fn step_retry_is_skipped_for_exhaustion_auth_and_stalls_only() {
        assert!(skips_step_retry("Credit balance is too low"));
        assert!(skips_step_retry("You've hit your weekly limit"));
        assert!(skips_step_retry("Error: session expired, please re-authenticate"));
        assert!(skips_step_retry(&format!(
            "Codex {} for 2700s while its output repeated",
            crate::agent_launch::STALLED_MARKER
        )));
        assert!(!skips_step_retry("429 Too Many Requests; Retry-After: 30"));
        assert!(!skips_step_retry("judge reply was not valid JSON"));
    }

    #[test]
    fn parse_duration_forms() {
        assert_eq!(parse_duration("30"), Some(30));
        assert_eq!(parse_duration(": 26.2s."), Some(27));
        assert_eq!(parse_duration("2h, 5m and 3s"), Some(2 * 3600 + 5 * 60 + 3));
        assert_eq!(parse_duration("a few minutes"), None);
        assert_eq!(parse_duration("5 apples"), None);
    }
}
