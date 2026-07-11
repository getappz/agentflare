// `agentflare hook session-start --agent X` / `agentflare hook prompt-submit --agent X`
// The runtime handlers — invoked by whatever `agentflare init` (or, for
// Codex, the plugin manifest) wired into the target agent's hook config.
// No install/consent logic lives here: `init` is the explicit, one-shot
// consent; these just reinforce rules and report drift each session/turn.
use crate::components::get_components;
use crate::state;
use serde_json::json;
use std::io::Read;
use std::time::Duration;

const STDIN_TIMEOUT_MS: u64 = 1000;

fn read_stdin_timeout(ms: u64) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut input = String::new();
        let _ = std::io::stdin().read_to_string(&mut input);
        let _ = tx.send(input);
    });
    rx.recv_timeout(Duration::from_millis(ms)).ok()
}

fn read_stdin_or_skip(label: &str) -> Option<String> {
    match read_stdin_timeout(STDIN_TIMEOUT_MS) {
        Some(s) if !s.is_empty() => Some(s),
        _ => {
            eprintln!("[agentflare] {label}: stdin timeout or empty — skipping");
            None
        }
    }
}

pub fn session_start(agent: &str) {
    println!("{}", session_start_message(agent));
}

fn session_start_message(agent: &str) -> String {
    let components = get_components(agent);
    let mut lines = vec![];
    let mut pending = vec![];

    for c in &components {
        if (c.check)() {
            continue;
        }
        if c.needs_consent {
            pending.push(&c.describe);
        } else {
            lines.push((c.apply)());
        }
    }

    if !pending.is_empty() {
        lines.push(String::new());
        lines.push(format!(
            "agentflare: the following aren't set up yet — run `agentflare init --agent {agent}` to install them:"
        ));
        for d in pending {
            lines.push(format!("  - {d}"));
        }
    }

    let rule_bodies = crate::coaching::active_rule_bodies();
    if !rule_bodies.is_empty() {
        lines.push(String::new());
        lines.push("Coaching rules:".to_string());
        for body in rule_bodies {
            lines.push(format!("  - {body}"));
        }
    }

    lines.push(String::new());
    lines.push(
        "AGENTFLARE ACTIVE — lean-ctx tools, Exa search, clean git commits. Off: /agentflare off."
            .to_string(),
    );
    lines.push(
        "Skills load on demand: before assuming a relevant skill doesn't exist, call skill_search(query) then skill_load(name) via the agentflare MCP tools."
            .to_string(),
    );
    lines.push(
        "Memory: agentflare's built-in memory_remember/memory_recall/memory_context/memory_handoff/memory_relate/memory_curate MCP tools (or `agentflare memory ...` CLI) — no install needed, call directly."
            .to_string(),
    );

    lines.join("\n")
}

fn extract_prompt(input: &str) -> String {
    serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|v| {
            v.get("prompt")
                .or_else(|| v.get("text"))
                .or_else(|| v.get("message"))
                .and_then(|p| p.as_str())
                .map(str::to_lowercase)
        })
        .unwrap_or_default()
}

struct PreToolUseInput {
    session_id: String,
    tool_name: String,
    delay_seconds: Option<u64>,
}

fn parse_pre_tool_use(input: &str) -> Option<PreToolUseInput> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    let tool_name = v.get("tool_name")?.as_str()?.to_string();
    let delay_seconds = v
        .get("tool_input")
        .and_then(|ti| ti.get("delaySeconds"))
        .and_then(|d| d.as_u64());
    Some(PreToolUseInput {
        session_id,
        tool_name,
        delay_seconds,
    })
}

pub fn pre_tool_use(_agent: &str) {
    let Some(input) = read_stdin_or_skip("PreToolUse") else {
        return;
    };
    let Some(parsed) = parse_pre_tool_use(&input) else {
        return;
    };

    let mut runtime = crate::optimize::load_runtime();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    crate::optimize::prune_stale_sessions(&mut runtime, now);

    let record = runtime
        .sessions
        .entry(parsed.session_id.clone())
        .or_insert_with(|| crate::optimize::SessionRecord {
            start_ts: now,
            turn_count: 0,
            recent_tool_calls: vec![],
        });

    let mut nudges: Vec<String> = vec![];

    if let Some(nudge) =
        crate::optimize::batching_nudge(&record.recent_tool_calls, &parsed.tool_name)
    {
        nudges.push(nudge);
    }

    if parsed.tool_name == "ScheduleWakeup"
        && let Some(delay) = parsed.delay_seconds
        && let Some(nudge) = crate::optimize::schedule_wakeup_nudge(delay)
    {
        nudges.push(nudge.to_string());
    }

    record
        .recent_tool_calls
        .push(crate::optimize::ToolCallRecord {
            name: parsed.tool_name.clone(),
            ts: now,
        });
    if record.recent_tool_calls.len() > 10 {
        record.recent_tool_calls.remove(0);
    }

    crate::optimize::save_runtime(&runtime);

    if !nudges.is_empty() {
        let out = json!({
            "systemMessage": format!("agentflare: {}", nudges.join(" "))
        });
        println!("{out}");
    }
}

/// No-op, kept only so a `settings.json` entry written by an older agentflare
/// version (which fired an `engram-cli` handoff here — removed along with the
/// rest of the engram integration) doesn't start erroring on every session
/// end after an upgrade. New installs never wire this hook (see init.rs).
pub fn session_end(_agent: &str) {}

pub fn prompt_submit(agent: &str) {
    let Some(input) = read_stdin_or_skip("UserPromptSubmit") else {
        return;
    };
    let prompt = extract_prompt(&input);
    let prompt = prompt.trim();

    let session_id: Option<String> = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| {
            v.get("session_id")
                .and_then(|s| s.as_str())
                .map(String::from)
        });

    let mut s = state::load();

    if prompt == "/agentflare" || prompt == "/agentflare status" {
        let state = if s.active { "ACTIVE" } else { "off" };
        let out = json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": format!("agentflare is {state}. Use /agentflare on | off | status."),
            }
        });
        println!("{out}");
        return;
    }
    if prompt == "/agentflare off" || prompt == "/agentflare stop" {
        s.active = false;
        state::save(&s);
        return;
    }
    if prompt == "/agentflare on" {
        s.active = true;
        state::save(&s);
    }

    if !s.active {
        return;
    }

    let mut bits = vec![
        "AGENTFLARE ACTIVE.".to_string(),
        "Prefer lean-ctx ctx_* tools over native Read/Grep/Bash/Glob.".to_string(),
        "Exa is the only web search tool.".to_string(),
        "Clean git commits, no AI signature.".to_string(),
    ];
    let pending = get_components(agent)
        .iter()
        .any(|c| c.needs_consent && !(c.check)());
    if pending {
        bits.push(format!(
            "Reminder: `agentflare init --agent {agent}` to finish setup."
        ));
    }

    let router = crate::optimize::active_router();

    if let Some(sid) = session_id {
        let mut runtime = crate::optimize::load_runtime();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        crate::optimize::prune_stale_sessions(&mut runtime, now);
        let record =
            runtime
                .sessions
                .entry(sid.clone())
                .or_insert_with(|| crate::optimize::SessionRecord {
                    start_ts: now,
                    turn_count: 0,
                    recent_tool_calls: vec![],
                });
        record.turn_count += 1;

        let ctx = crate::optimize::RouteContext {
            prompt: prompt.to_string(),
            session_id: sid,
            turn_count: record.turn_count,
            recent_tool_calls: record.recent_tool_calls.clone(),
            current_model: None,
        };
        if let Some(nudge) = router.route(&ctx) {
            bits.push(nudge);
        }

        if let Some(nudge) = crate::optimize::session_hygiene_nudge(record, now) {
            bits.push(nudge);
        }
        crate::optimize::save_runtime(&runtime);
    } else {
        let ctx = crate::optimize::RouteContext {
            prompt: prompt.to_string(),
            session_id: String::new(),
            turn_count: 0,
            recent_tool_calls: vec![],
            current_model: None,
        };
        if let Some(nudge) = router.route(&ctx) {
            bits.push(nudge);
        }
    }

    let out = json!({
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": bits.join(" "),
        }
    });
    println!("{out}");
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;

    #[test]
    fn read_stdin_timeout_does_not_panic() {
        let _ = read_stdin_timeout(100);
    }

    #[test]
    fn extract_prompt_reads_prompt_key() {
        assert_eq!(
            extract_prompt(r#"{"prompt": "Hello World"}"#),
            "hello world"
        );
    }

    #[test]
    fn extract_prompt_falls_back_to_text_key() {
        assert_eq!(extract_prompt(r#"{"text": "Foo"}"#), "foo");
    }

    #[test]
    fn extract_prompt_falls_back_to_message_key() {
        assert_eq!(extract_prompt(r#"{"message": "Bar"}"#), "bar");
    }

    #[test]
    fn extract_prompt_prefers_prompt_over_text_and_message() {
        assert_eq!(
            extract_prompt(r#"{"prompt": "A", "text": "B", "message": "C"}"#),
            "a"
        );
    }

    #[test]
    fn extract_prompt_returns_empty_on_invalid_json() {
        assert_eq!(extract_prompt("not json"), "");
    }

    #[test]
    fn extract_prompt_returns_empty_when_no_known_key() {
        assert_eq!(extract_prompt(r#"{"other": "value"}"#), "");
    }

    #[test]
    fn parse_pre_tool_use_reads_session_and_tool_name() {
        let input = r#"{"session_id": "abc", "tool_name": "Read", "tool_input": {}}"#;
        let parsed = parse_pre_tool_use(input).unwrap();
        assert_eq!(parsed.session_id, "abc");
        assert_eq!(parsed.tool_name, "Read");
        assert_eq!(parsed.delay_seconds, None);
    }

    #[test]
    fn parse_pre_tool_use_reads_delay_seconds_for_schedule_wakeup() {
        let input = r#"{"session_id": "abc", "tool_name": "ScheduleWakeup", "tool_input": {"delaySeconds": 280}}"#;
        let parsed = parse_pre_tool_use(input).unwrap();
        assert_eq!(parsed.delay_seconds, Some(280));
    }

    #[test]
    fn parse_pre_tool_use_returns_none_on_invalid_json() {
        assert!(parse_pre_tool_use("not json").is_none());
    }

    #[test]
    fn session_start_includes_active_coaching_rule_bodies() {
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            crate::coaching::apply_rule(
                "hygiene",
                "Close sessions promptly",
                "Wrap up each phase before starting the next.",
            )
            .unwrap();

            // session_start prints via println!; session_start_message
            // (below) covers the actual message content, this just confirms
            // the printing entry point doesn't panic when a coaching rule is
            // active. The underlying data source's correctness (ordering,
            // content) is covered by Task 1's active_rule_bodies tests in
            // coaching.rs.
            session_start("claude-code");

            let bodies = crate::coaching::active_rule_bodies();
            assert_eq!(
                bodies,
                vec!["Wrap up each phase before starting the next.".to_string()]
            );
        });
    }

    #[test]
    fn session_start_message_nudges_skill_search_before_load() {
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            let msg = session_start_message("claude-code");
            assert!(msg.contains("skill_search(query)"));
            assert!(msg.contains("skill_load(name)"));
        });
    }

    #[test]
    fn session_start_message_points_to_builtin_memory_tools() {
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            let msg = session_start_message("claude-code");
            assert!(msg.contains("memory_remember"));
            assert!(msg.contains("memory_recall"));
        });
    }
}
