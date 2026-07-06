// `agentflare hook session-start --agent X` / `agentflare hook prompt-submit --agent X`
// The runtime handlers — invoked by whatever `agentflare init` (or, for
// Codex, the plugin manifest) wired into the target agent's hook config.
// No install/consent logic lives here: `init` is the explicit, one-shot
// consent; these just reinforce rules and report drift each session/turn.
use crate::components::get_components;
use crate::state;
use serde_json::json;
use std::io::Read;

pub fn session_start(agent: &str) {
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
        "AGENTFLARE ACTIVE — lean-ctx/engram tools, Exa search, clean git commits. Off: /agentflare off."
            .to_string(),
    );

    println!("{}", lines.join("\n"));
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
    Some(PreToolUseInput { session_id, tool_name, delay_seconds })
}

pub fn pre_tool_use(_agent: &str) {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let Some(parsed) = parse_pre_tool_use(&input) else { return };

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

    if let Some(nudge) = crate::optimize::batching_nudge(&record.recent_tool_calls, &parsed.tool_name) {
        nudges.push(nudge);
    }

    if parsed.tool_name == "ScheduleWakeup" {
        if let Some(delay) = parsed.delay_seconds {
            if let Some(nudge) = crate::optimize::schedule_wakeup_nudge(delay) {
                nudges.push(nudge.to_string());
            }
        }
    }

    record.recent_tool_calls.push(crate::optimize::ToolCallRecord {
        name: parsed.tool_name.clone(),
        ts: now,
    });
    if record.recent_tool_calls.len() > 10 {
        record.recent_tool_calls.remove(0);
    }

    crate::optimize::save_runtime(&runtime);

    if !nudges.is_empty() {
        let out = json!({
            "systemMessage": nudges.join(" ")
        });
        println!("{out}");
    }
}

pub fn prompt_submit(agent: &str) {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let prompt = extract_prompt(&input);
    let prompt = prompt.trim();

    let session_id: Option<String> = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| v.get("session_id").and_then(|s| s.as_str()).map(String::from));

    let mut s = state::load();

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
        bits.push(format!("Reminder: `agentflare init --agent {agent}` to finish setup."));
    }

    if let Some(nudge) = crate::optimize::model_routing_nudge(prompt) {
        bits.push(nudge.to_string());
    }

    if let Some(sid) = session_id {
        let mut runtime = crate::optimize::load_runtime();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        crate::optimize::prune_stale_sessions(&mut runtime, now);
        let record = runtime
            .sessions
            .entry(sid)
            .or_insert_with(|| crate::optimize::SessionRecord {
                start_ts: now,
                turn_count: 0,
                recent_tool_calls: vec![],
            });
        record.turn_count += 1;
        if let Some(nudge) = crate::optimize::session_hygiene_nudge(record, now) {
            bits.push(nudge);
        }
        crate::optimize::save_runtime(&runtime);
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
mod tests {
    use super::*;

    #[test]
    fn extract_prompt_reads_prompt_key() {
        assert_eq!(extract_prompt(r#"{"prompt": "Hello World"}"#), "hello world");
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
        assert_eq!(extract_prompt(r#"{"prompt": "A", "text": "B", "message": "C"}"#), "a");
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
            crate::coaching::apply_rule("hygiene", "Close sessions promptly", "Wrap up each phase before starting the next.").unwrap();

            // session_start prints via println!, not a return value — this
            // confirms the actual integration point doesn't panic when a
            // coaching rule is active. The underlying data source's
            // correctness (ordering, content) is covered by Task 1's
            // active_rule_bodies tests in coaching.rs.
            session_start("claude-code");

            let bodies = crate::coaching::active_rule_bodies();
            assert_eq!(bodies, vec!["Wrap up each phase before starting the next.".to_string()]);
        });
    }
}
