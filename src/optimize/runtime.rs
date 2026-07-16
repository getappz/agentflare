use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct RuntimeState {
    #[serde(default)]
    pub sessions: HashMap<String, SessionRecord>,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct SessionRecord {
    pub start_ts: u64,
    #[serde(default)]
    pub turn_count: u32,
    #[serde(default)]
    pub recent_tool_calls: Vec<ToolCallRecord>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct ToolCallRecord {
    pub name: String,
    pub ts: u64,
}

const STALE_SESSION_SECS: u64 = 24 * 60 * 60;

pub fn runtime_state_path() -> PathBuf {
    crate::state::state_dir().join("runtime-state.json")
}

pub fn load_runtime() -> RuntimeState {
    fs::read_to_string(runtime_state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_runtime(state: &RuntimeState) {
    let _ = fs::create_dir_all(crate::state::state_dir());
    if let Ok(json) = serde_json::to_string_pretty(state) {
        let _ = fs::write(runtime_state_path(), json + "\n");
    }
}

pub fn prune_stale_sessions(state: &mut RuntimeState, now: u64) {
    state
        .sessions
        .retain(|_, record| now.saturating_sub(record.start_ts) < STALE_SESSION_SECS);
}

pub const SESSION_HYGIENE_TURN_THRESHOLD: u32 = 80;
pub const SESSION_HYGIENE_TIME_THRESHOLD_SECS: u64 = 2 * 60 * 60;

pub fn session_hygiene_nudge(record: &SessionRecord, now: u64) -> Option<String> {
    let elapsed = now.saturating_sub(record.start_ts);
    if record.turn_count < SESSION_HYGIENE_TURN_THRESHOLD
        && elapsed < SESSION_HYGIENE_TIME_THRESHOLD_SECS
    {
        return None;
    }
    Some(format!(
        "This session has run {} turns over {}h — consider closing it (handoff + fresh session) before context re-reads get expensive.",
        record.turn_count,
        elapsed / 3600
    ))
}

const LOCATE_KEYWORDS: &[&str] = &["find ", "where is", "where's", "search for", "locate "];

fn has_word_boundary_match(text: &str, keyword: &str) -> bool {
    let bytes = text.as_bytes();
    let mut start = 0;
    while let Some(pos) = text[start..].find(keyword) {
        let abs_pos = start + pos;
        let preceded_ok = abs_pos == 0 || !bytes[abs_pos - 1].is_ascii_alphabetic();
        if preceded_ok {
            return true;
        }
        start = abs_pos + keyword.len().max(1);
        if start > text.len() {
            break;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Router trait — pluggable per-call model routing nudges
// ---------------------------------------------------------------------------

/// Context available to every routing decision.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RouteContext {
    pub prompt: String,
    pub session_id: String,
    pub turn_count: u32,
    pub recent_tool_calls: Vec<ToolCallRecord>,
    pub current_model: Option<String>,
}

/// A router decides whether to suggest a different model (or operational
/// nudge) for a single LLM call. Return `None` to stay silent.
pub trait Router: Send + Sync {
    fn route(&self, ctx: &RouteContext) -> Option<String>;
}

// ---------------------------------------------------------------------------
// KeywordRouter — the existing keyword heuristic as a Router
// ---------------------------------------------------------------------------

pub struct KeywordRouter;

impl Router for KeywordRouter {
    fn route(&self, ctx: &RouteContext) -> Option<String> {
        model_routing_nudge(&ctx.prompt).map(|s| s.to_string())
    }
}

// ---------------------------------------------------------------------------
// LengthBasedRouter — short prompts → cheap model; long → keep current
// ---------------------------------------------------------------------------

const SHORT_PROMPT_LEN: usize = 100;
const LONG_PROMPT_LEN: usize = 2000;

pub struct LengthBasedRouter {
    pub cheap_model: String,
    pub big_model: String,
}

impl Router for LengthBasedRouter {
    fn route(&self, ctx: &RouteContext) -> Option<String> {
        let len = ctx.prompt.chars().count();
        if len < SHORT_PROMPT_LEN {
            Some(format!(
                "Short prompt ({len} chars) — consider routing to {}.",
                self.cheap_model
            ))
        } else if len > LONG_PROMPT_LEN {
            Some(format!(
                "Long prompt ({len} chars) — stick with {} for quality.",
                self.big_model
            ))
        } else {
            None
        }
    }
}

/// Select a router by name. Unknown/empty names fall back to `KeywordRouter`,
/// preserving today's behavior when nothing is configured.
pub fn router_by_name(name: &str) -> Box<dyn Router> {
    match name {
        "length" => Box::new(LengthBasedRouter {
            cheap_model: "haiku".to_string(),
            big_model: "opus".to_string(),
        }),
        _ => Box::new(KeywordRouter),
    }
}

/// The router hook.rs actually uses, selected via `AGENTFLARE_ROUTER` (unset
/// or unrecognized -> `KeywordRouter`, today's default behavior).
pub fn active_router() -> Box<dyn Router> {
    let name = std::env::var("AGENTFLARE_ROUTER").unwrap_or_default();
    router_by_name(&name)
}

/// Legacy free-function — delegates to `KeywordRouter`. Kept for backward
/// compatibility with existing test assertions.
pub fn model_routing_nudge(prompt: &str) -> Option<&'static str> {
    let lower = prompt.to_lowercase();
    if LOCATE_KEYWORDS
        .iter()
        .any(|kw| has_word_boundary_match(&lower, kw))
    {
        return Some(
            "This looks like a locate/investigate task — consider a cheap-model subagent (e.g. haiku) instead of running it inline.",
        );
    }
    None
}

pub const BATCHABLE_TOOLS: &[&str] = &["Read", "Bash", "ctx_read", "ctx_shell"];
const BATCH_WINDOW: usize = 3;

/// Flags when the last BATCH_WINDOW calls (including the one about to run)
/// are all solo calls to the same batch-eligible tool — a sign a batch form
/// should have been used instead.
pub fn batching_nudge(recent_calls: &[ToolCallRecord], next_tool: &str) -> Option<String> {
    if !BATCHABLE_TOOLS.contains(&next_tool) {
        return None;
    }
    let tail: Vec<&str> = recent_calls
        .iter()
        .rev()
        .take(BATCH_WINDOW - 1)
        .map(|c| c.name.as_str())
        .collect();
    if tail.len() < BATCH_WINDOW - 1 {
        return None;
    }
    if tail.iter().all(|&name| name == next_tool) {
        return Some(format!(
            "That's {} consecutive solo calls to {next_tool} — check if it accepts a batch/list form instead.",
            BATCH_WINDOW
        ));
    }
    None
}

pub fn schedule_wakeup_nudge(delay_seconds: u64) -> Option<&'static str> {
    if (271..300).contains(&delay_seconds) {
        return Some(
            "This delay is in the cache-miss dead zone (271-299s) — drop under 270s to stay in cache, or extend past 1200s to make the miss worth it.",
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn load_defaults_to_empty_when_no_file() {
        with_temp_home(|| {
            assert!(load_runtime().sessions.is_empty());
        });
    }

    #[test]
    fn save_then_load_roundtrips() {
        with_temp_home(|| {
            let mut state = RuntimeState::default();
            state.sessions.insert(
                "sess-1".to_string(),
                SessionRecord {
                    start_ts: 1000,
                    turn_count: 3,
                    recent_tool_calls: vec![],
                },
            );
            save_runtime(&state);
            let loaded = load_runtime();
            assert_eq!(loaded.sessions.len(), 1);
            assert_eq!(loaded.sessions["sess-1"].turn_count, 3);
        });
    }

    #[test]
    fn load_falls_back_to_default_on_corrupt_file() {
        with_temp_home(|| {
            fs::create_dir_all(crate::state::state_dir()).unwrap();
            fs::write(runtime_state_path(), "not json").unwrap();
            assert!(load_runtime().sessions.is_empty());
        });
    }

    #[test]
    fn prune_drops_sessions_older_than_24h_keeps_recent() {
        let mut state = RuntimeState::default();
        state.sessions.insert(
            "old".to_string(),
            SessionRecord {
                start_ts: 0,
                turn_count: 1,
                recent_tool_calls: vec![],
            },
        );
        state.sessions.insert(
            "recent".to_string(),
            SessionRecord {
                start_ts: 100_000,
                turn_count: 1,
                recent_tool_calls: vec![],
            },
        );
        let now = 100_100; // 100s after "recent", ~27.7h after "old"
        prune_stale_sessions(&mut state, now);
        assert!(!state.sessions.contains_key("old"));
        assert!(state.sessions.contains_key("recent"));
    }

    #[test]
    fn session_hygiene_no_nudge_below_thresholds() {
        let record = SessionRecord {
            start_ts: 0,
            turn_count: 5,
            recent_tool_calls: vec![],
        };
        assert!(session_hygiene_nudge(&record, 100).is_none());
    }

    #[test]
    fn session_hygiene_nudges_past_turn_threshold() {
        let record = SessionRecord {
            start_ts: 0,
            turn_count: 81,
            recent_tool_calls: vec![],
        };
        assert!(session_hygiene_nudge(&record, 100).is_some());
    }

    #[test]
    fn session_hygiene_nudges_past_time_threshold() {
        let record = SessionRecord {
            start_ts: 0,
            turn_count: 1,
            recent_tool_calls: vec![],
        };
        assert!(session_hygiene_nudge(&record, 2 * 60 * 60 + 1).is_some());
    }

    #[test]
    fn model_routing_flags_find_prompts() {
        assert!(model_routing_nudge("find the auth handler").is_some());
    }

    #[test]
    fn model_routing_flags_where_is_prompts() {
        assert!(model_routing_nudge("where is the config loaded?").is_some());
    }

    #[test]
    fn model_routing_ignores_unrelated_prompts() {
        assert!(model_routing_nudge("refactor the payment module for clarity").is_none());
    }

    #[test]
    fn model_routing_ignores_locate_as_substring_of_allocate_words() {
        assert!(model_routing_nudge("let's reallocate the buffer").is_none());
        assert!(model_routing_nudge("we need to allocate more memory").is_none());
        assert!(model_routing_nudge("relocate the config file later").is_none());
    }

    #[test]
    fn model_routing_ignores_where_is_as_substring_of_other_words() {
        assert!(model_routing_nudge("documented elsewhere is fine").is_none());
        assert!(model_routing_nudge("it works nowhere is that a problem").is_none());
    }

    #[test]
    fn model_routing_still_flags_real_locate_and_where_is_prompts() {
        assert!(model_routing_nudge("please locate the missing file").is_some());
        assert!(model_routing_nudge("where is the auth check?").is_some());
    }

    #[test]
    fn batching_flags_three_consecutive_solo_calls() {
        let recent = vec![
            ToolCallRecord {
                name: "Read".to_string(),
                ts: 1,
            },
            ToolCallRecord {
                name: "Read".to_string(),
                ts: 2,
            },
        ];
        assert!(batching_nudge(&recent, "Read").is_some());
    }

    #[test]
    fn batching_ignores_non_batchable_tool() {
        let recent = vec![
            ToolCallRecord {
                name: "Write".to_string(),
                ts: 1,
            },
            ToolCallRecord {
                name: "Write".to_string(),
                ts: 2,
            },
        ];
        assert!(batching_nudge(&recent, "Write").is_none());
    }

    #[test]
    fn batching_ignores_mixed_recent_calls() {
        let recent = vec![
            ToolCallRecord {
                name: "Read".to_string(),
                ts: 1,
            },
            ToolCallRecord {
                name: "Grep".to_string(),
                ts: 2,
            },
        ];
        assert!(batching_nudge(&recent, "Read").is_none());
    }

    #[test]
    fn batching_ignores_short_history() {
        let recent = vec![ToolCallRecord {
            name: "Read".to_string(),
            ts: 1,
        }];
        assert!(batching_nudge(&recent, "Read").is_none());
    }

    #[test]
    fn schedule_wakeup_nudges_dead_zone() {
        assert!(schedule_wakeup_nudge(280).is_some());
    }

    #[test]
    fn schedule_wakeup_silent_under_dead_zone() {
        assert!(schedule_wakeup_nudge(200).is_none());
    }

    #[test]
    fn schedule_wakeup_silent_over_dead_zone() {
        assert!(schedule_wakeup_nudge(1500).is_none());
    }

    // ---- Router trait tests ----

    fn ctx(prompt: &str) -> RouteContext {
        RouteContext {
            prompt: prompt.to_string(),
            session_id: "test-sess".to_string(),
            turn_count: 0,
            recent_tool_calls: vec![],
            current_model: None,
        }
    }

    #[test]
    fn keyword_router_flags_find_prompts() {
        let r = KeywordRouter;
        assert!(r.route(&ctx("find the auth handler")).is_some());
    }

    #[test]
    fn keyword_router_ignores_unrelated_prompts() {
        let r = KeywordRouter;
        assert!(r.route(&ctx("refactor the payment module")).is_none());
    }

    #[test]
    fn keyword_router_ignores_locate_substring() {
        let r = KeywordRouter;
        assert!(r.route(&ctx("let's reallocate the buffer")).is_none());
    }

    #[test]
    fn length_router_short_prompt_routes_to_cheap() {
        let r = LengthBasedRouter {
            cheap_model: "haiku".into(),
            big_model: "opus".into(),
        };
        let nudge = r.route(&ctx("hi")).unwrap();
        assert!(nudge.contains("haiku"));
    }

    #[test]
    fn length_router_long_prompt_sticks_to_big() {
        let r = LengthBasedRouter {
            cheap_model: "haiku".into(),
            big_model: "opus".into(),
        };
        let long = "x".repeat(2500);
        let nudge = r.route(&ctx(&long)).unwrap();
        assert!(nudge.contains("opus"));
    }

    #[test]
    fn length_router_medium_prompt_silent() {
        let r = LengthBasedRouter {
            cheap_model: "haiku".into(),
            big_model: "opus".into(),
        };
        let medium = "x".repeat(500);
        assert!(r.route(&ctx(&medium)).is_none());
    }

    #[test]
    fn router_trait_is_object_safe() {
        let r: &dyn Router = &KeywordRouter;
        assert!(r.route(&ctx("find me")).is_some());
    }

    #[test]
    fn router_by_name_defaults_to_keyword_router() {
        let r = router_by_name("");
        // KeywordRouter is silent on prompts with no locate-style keywords.
        assert!(r.route(&ctx("refactor the payment module")).is_none());
        // ...but flags them, same as KeywordRouter directly.
        assert!(
            router_by_name("nonsense")
                .route(&ctx("find the auth handler"))
                .is_some()
        );
    }

    #[test]
    fn router_by_name_length_selects_length_based_router() {
        let r = router_by_name("length");
        let nudge = r.route(&ctx("hi")).unwrap();
        assert!(nudge.contains("haiku"));
    }
}
