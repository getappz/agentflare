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

// Brand palette — same codes as `banner::colorize` (magenta wordmark, cyan
// dividers) so hook output matches the CLI. Gated on `NO_COLOR` only, not
// `interactive()`: hook stdout is always a pipe back to the host agent, never
// the real terminal, so a TTY check here would just disable color outright.
const BRAND: &str = "\x1b[1;35m";
const HEADING: &str = "\x1b[2;36m";
const RESET: &str = "\x1b[0m";

fn colorize(code: &str, s: &str) -> String {
    if std::env::var_os("NO_COLOR").is_some() {
        s.to_string()
    } else {
        format!("{code}{s}{RESET}")
    }
}

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
    let msg = session_start_message(agent);

    // Flush any vents buffered since the last turn/session (best-effort;
    // never blocks the hook or surfaces errors to the agent).
    let _ = std::panic::catch_unwind(|| {
        let r = crate::vent::consolidate::consolidate();
        if !r.items_created.is_empty() {
            eprintln!(
                "[agentflare] vent: filed {} item(s) from friction",
                r.items_created.len()
            );
        }
    });

    // Plain stdout reaches Claude's context for this event (see module
    // comment) but is NOT shown to the user in the terminal. `systemMessage`
    // is the only field that renders visibly, so emit both: the user sees
    // it, and Claude still gets it via additionalContext.
    let out = json!({
        "systemMessage": msg,
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": msg,
        }
    });
    println!("{out}");
}

fn session_start_message(agent: &str) -> String {
    let components = get_components(agent);
    let is_active = |id: &str| components.iter().any(|c| c.id == id && (c.check)());
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
        lines.push(colorize(
            HEADING,
            &format!("agentflare: setup needed — run `agentflare init --agent {agent}`:"),
        ));
        for d in pending {
            lines.push(format!("  - {d}"));
        }
    }

    let rule_bodies = crate::coaching::untriggered_rule_bodies();
    if !rule_bodies.is_empty() {
        lines.push(String::new());
        lines.push(colorize(HEADING, "Coaching rules:"));
        for body in rule_bodies {
            lines.push(format!("  - {body}"));
        }
    }

    // Pending item queue: items assigned to this agent that are still open.
    // Resolves the SAME project this repo's other agentflare features
    // (item/artifact/comment, ...) auto-link to -- NOT just "whichever
    // project happens to be oldest in backend.db", which is a single
    // shared database across every repo agentflare has ever touched on
    // this machine.
    let db_path = crate::paths::home().join(".agentflare").join("backend.db");
    if db_path.exists()
        && let Ok(conn) = agentflare_backend::db::open_db(&db_path)
        && let Some(pid) = crate::mcp_server::AgentflareMcp::default()
            .resolve_project(&conn)
            .ok()
            .map(|p| p.id)
        && let Ok(items) = agentflare_backend::item::list_by_assignee_agent(&conn, &pid, agent)
        && !items.is_empty()
    {
        lines.push(String::new());
        lines.push(colorize(
            HEADING,
            &format!(
                "Pending items assigned to you ({agent}, {} open):",
                items.len()
            ),
        ));
        const MAX_SHOWN: usize = 10;
        for item in items.iter().take(MAX_SHOWN) {
            lines.push(format!("  #{} {}", item.sequence_id, item.name));
        }
        if items.len() > MAX_SHOWN {
            lines.push(format!("  ... and {} more", items.len() - MAX_SHOWN));
        }
    }

    // Skill preload: detect project context and surface relevant skills.
    let project_queries = crate::skill_detect::session_context_queries();
    if !project_queries.is_empty() {
        let db_path = crate::paths::skills_db_path();
        if db_path.exists()
            && let Ok(mut registry) = skill_registry::Registry::open_default(&db_path)
        {
            let _ = registry.ensure_fresh(crate::components::detected_skill_agents);
            for q in &project_queries {
                if let Ok(hits) = registry.search(q, 2, skill_registry::MatchMode::Any)
                    && !hits.is_empty()
                {
                    let names: Vec<_> = hits.iter().map(|h| h.name.as_str()).collect();
                    lines.push(format!("  {} skills: {}", q, names.join(", ")));
                }
            }
        }
    }

    // Closing status line: names only the tools this session can actually
    // reach — each tag is gated on the matching component's own `check()` so
    // it never claims a feature that isn't wired up. (Previously this was a
    // static line claiming lean-ctx/Exa/MCP tools were active regardless of
    // whether `agentflare init` had ever registered them.)
    let mut tags = vec![];
    if is_active("leanctx") {
        tags.push("lean-ctx ctx_* tools");
    }
    if is_active("rules") {
        tags.push("Exa search, clean commits");
    }
    if is_active("agentflare-mcp") {
        tags.push("skill_search/skill_load + memory_* MCP tools");
    }
    lines.push(String::new());
    lines.push(if tags.is_empty() {
        format!(
            "{} — nothing active yet, see setup above.",
            colorize(BRAND, "agentflare")
        )
    } else {
        format!(
            "{} active — {}. /agentflare off to disable.",
            colorize(BRAND, "agentflare"),
            tags.join("; ")
        )
    });

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
    tool_input: Option<serde_json::Value>,
    delay_seconds: Option<u64>,
}

fn parse_pre_tool_use(input: &str) -> Option<PreToolUseInput> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    let tool_name = v.get("tool_name")?.as_str()?.to_string();
    let tool_input = v.get("tool_input").cloned();
    let delay_seconds = tool_input
        .as_ref()
        .and_then(|ti| ti.get("delaySeconds"))
        .and_then(|d| d.as_u64());
    Some(PreToolUseInput {
        session_id,
        tool_name,
        tool_input,
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

    if let Some(decision) =
        crate::hook_redirect::redirect_decision(&parsed.tool_name, parsed.tool_input.as_ref())
    {
        println!("{decision}");
        return;
    }

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

    nudges.extend(crate::coaching::rule_bodies_for_tool(&parsed.tool_name));

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

#[allow(dead_code)]
struct PreCompactInput {
    session_id: String,
    transcript_path: Option<String>,
    trigger: String,
}

#[allow(dead_code)]
fn parse_pre_compact(input: &str) -> Option<PreCompactInput> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    let session_id = v.get("session_id")?.as_str()?.to_string();
    let transcript_path = v
        .get("transcript_path")
        .and_then(|t| t.as_str())
        .map(String::from);
    let trigger = v
        .get("trigger")
        .and_then(|t| t.as_str())
        .unwrap_or("auto")
        .to_string();
    Some(PreCompactInput {
        session_id,
        transcript_path,
        trigger,
    })
}

/// `session_id` is a UUID, not natural-language text -- FTS5-matching it
/// against transcript content would essentially never hit, making
/// relevance scoring a no-op. Use the transcript's own last non-empty
/// line (the most recent turn) as a proxy for what the user is currently
/// focused on, so older lines get ranked by relevance to that instead of
/// to an opaque session identifier. Falls back to `session_id` only when
/// the whole transcript is empty.
#[allow(dead_code)]
fn relevance_query<'a>(content: &'a str, session_id: &'a str) -> &'a str {
    content
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(session_id)
}

/// DEPRECATED / unsupported no-op. This used to score transcript lines by
/// FTS5/BM25 relevance and print them as JSON, on the theory that Claude
/// Code's compaction would prioritise keeping relevant context. It never
/// did anything: Claude Code's PreCompact hook is blocking-only and does
/// not consume `hookSpecificOutput.additionalContext`, so the scored
/// output was discarded unread. Compaction-survival is now handled
/// end-to-end by the lean-ctx sidecar (PostToolUse state accumulation +
/// SessionStart re-injection), which agentflare should not duplicate.
///
/// Kept as a stub (rather than deleted) so existing `settings.json`
/// PreCompact wiring from prior installs doesn't start erroring after an
/// upgrade, and so `parse_pre_compact`/`relevance_query` above stay
/// available if compaction-survival is ever reactivated here. The FTS5
/// scorer itself (`crate::compact::score_lines`) is not dead code — it's
/// still used by the coaching-rule digest (`coaching::store`).
pub fn pre_compact(_agent: &str) {}

/// No-op, kept only so a `settings.json` entry written by an older agentflare
/// version (which fired an `engram-cli` handoff here — removed along with the
/// rest of the engram integration) doesn't start erroring on every session
/// end after an upgrade. New installs never wire this hook (see init.rs).
pub fn session_end(_agent: &str) {}

/// Static identity/rules reminder — sent once per session (first turn only,
/// not every turn) and gated on which components are actually active, so it
/// never claims a tool that isn't wired up. Uses the same terse `@tag:`
/// syntax as the rule files in `rule_text.rs` instead of full sentences.
fn identity_bits(components: &[crate::components::Component]) -> Vec<String> {
    let is_active = |id: &str| components.iter().any(|c| c.id == id && (c.check)());
    let mut bits = vec!["@agentflare: active — /agentflare off to disable".to_string()];
    if is_active("leanctx") {
        bits.push("@use: lean-ctx ctx_* over native Read/Grep/Bash/Glob".to_string());
    }
    if is_active("rules") {
        bits.push("@use: Exa for web search; clean git commits, no AI signature".to_string());
    }
    bits
}

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

    // Triage the previous turn's buffered vents once per turn (best-effort;
    // never blocks the hook or surfaces errors to the agent).
    let _ = std::panic::catch_unwind(|| {
        let r = crate::vent::consolidate::consolidate();
        if !r.items_created.is_empty() {
            eprintln!(
                "[agentflare] vent: filed {} item(s) from friction",
                r.items_created.len()
            );
        }
    });

    let router = crate::optimize::active_router();
    let mut session_bits = vec![];
    // No session_id to track turn count against (rare) — always remind, same
    // as a first turn.
    let mut first_turn = session_id.is_none();

    if let Some(sid) = &session_id {
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
        first_turn = record.turn_count == 0;
        record.turn_count += 1;

        let ctx = crate::optimize::RouteContext {
            prompt: prompt.to_string(),
            session_id: sid.clone(),
            turn_count: record.turn_count,
            recent_tool_calls: record.recent_tool_calls.clone(),
            current_model: None,
        };
        if let Some(nudge) = router.route(&ctx) {
            session_bits.push(nudge);
        }

        if let Some(nudge) = crate::optimize::session_hygiene_nudge(record, now) {
            session_bits.push(nudge);
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
            session_bits.push(nudge);
        }
    }

    let components = get_components(agent);
    let mut bits = if first_turn {
        identity_bits(&components)
    } else {
        vec![]
    };
    if let Some(block) = crate::mentions::expand(prompt) {
        bits.push(block);
    }
    let pending = components.iter().any(|c| c.needs_consent && !(c.check)());
    if pending {
        bits.push(format!("@setup: agentflare init --agent {agent}"));
    }
    bits.extend(session_bits);
    bits.extend(crate::coaching::rule_bodies_for_prompt(prompt));

    let intent = crate::skill_detect::classify(prompt);
    bits.push(crate::skill_detect::format_briefing_header(&intent));

    if intent.confidence >= 0.5 {
        let db_path = crate::paths::skills_db_path();
        if db_path.exists()
            && let Ok(mut registry) = skill_registry::Registry::open_default(&db_path)
        {
            let _ = registry.ensure_fresh(crate::components::detected_skill_agents);
            if let Ok(skills) = crate::skill_detect::find_skills(
                &intent,
                &registry,
                3,
                crate::memory::engine::embed_query,
                crate::memory::engine::embed_doc,
            ) && let Some(injection) = crate::skill_detect::build_injection(&skills)
            {
                bits.push(injection);
            }
        }
    }

    // Detect "install <something> skill" patterns → suggest CLI command.
    let q = prompt.to_lowercase();
    if q.contains("install") && (q.contains("skill") || q.contains("skills")) {
        let name = q
            .replace("install", "")
            .replace("skill", "")
            .replace("the", "")
            .replace("a", "")
            .replace("please", "")
            .trim()
            .to_string();
        if !name.is_empty() {
            bits.push(format!(
                "@skill-install: agentflare skill install \"{name}\" to search and install"
            ));
        } else {
            bits.push(
                "@skill-install: agentflare skill search <query> to find and install skills"
                    .to_string(),
            );
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
    fn parse_pre_compact_reads_session_and_trigger() {
        let input = r#"{"session_id": "abc", "transcript_path": "/x.jsonl", "hook_event_name": "PreCompact", "trigger": "auto"}"#;
        let parsed = parse_pre_compact(input).unwrap();
        assert_eq!(parsed.session_id, "abc");
        assert_eq!(parsed.transcript_path.as_deref(), Some("/x.jsonl"));
        assert_eq!(parsed.trigger, "auto");
    }

    #[test]
    fn parse_pre_compact_defaults_trigger_when_missing() {
        let input = r#"{"session_id": "abc", "hook_event_name": "PreCompact"}"#;
        let parsed = parse_pre_compact(input).unwrap();
        assert_eq!(parsed.trigger, "auto");
        assert!(parsed.transcript_path.is_none());
    }

    #[test]
    fn parse_pre_compact_rejects_invalid_json() {
        assert!(parse_pre_compact("not json").is_none());
    }

    #[test]
    fn parse_pre_compact_rejects_missing_session() {
        let input = r#"{"trigger": "manual"}"#;
        assert!(parse_pre_compact(input).is_none());
    }

    #[test]
    fn relevance_query_uses_last_non_empty_transcript_line() {
        let content = "first line
second line

";
        assert_eq!(relevance_query(content, "some-session-uuid"), "second line");
    }

    #[test]
    fn relevance_query_falls_back_to_session_id_when_transcript_empty() {
        assert_eq!(
            relevance_query("", "some-session-uuid"),
            "some-session-uuid"
        );
        assert_eq!(
            relevance_query(
                "   
	
",
                "some-session-uuid"
            ),
            "some-session-uuid"
        );
    }

    #[test]
    fn parse_pre_tool_use_returns_none_on_invalid_json() {
        assert!(parse_pre_tool_use("not json").is_none());
    }

    #[test]
    fn session_start_includes_untriggered_coaching_rule_bodies() {
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            crate::coaching::apply_rule(
                "hygiene",
                "Close sessions promptly",
                "Wrap up each phase before starting the next.",
                None,
            )
            .unwrap();

            session_start("claude-code");

            let bodies = crate::coaching::untriggered_rule_bodies();
            assert_eq!(
                bodies,
                vec!["Wrap up each phase before starting the next.".to_string()]
            );
        });
    }

    #[test]
    fn session_start_message_names_skill_and_memory_tools_when_mcp_registered() {
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            std::fs::write(
                crate::paths::claude_json_path(),
                r#"{"mcpServers": {"flare": {"command": "agentflare", "args": ["mcp"]}}}"#,
            )
            .unwrap();

            let msg = session_start_message("claude-code");
            assert!(msg.contains("skill_search/skill_load"));
            assert!(msg.contains("memory_*"));
        });
    }

    #[test]
    fn session_start_message_does_not_claim_unregistered_mcp_tools() {
        // Regression test: the closing status line must never claim a
        // feature is active when its component's own `check()` says
        // otherwise — a fresh $HOME has no `flare` MCP server registered, so
        // skill/memory tooling must not be mentioned as active. (The pending
        // setup section legitimately mentions "skill_search/skill_load" in
        // its describe text, so assert on the full active-tag phrase rather
        // than that substring alone.)
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            let msg = session_start_message("claude-code");
            assert!(!msg.contains("skill_search/skill_load + memory_*"));
            assert!(!msg.contains("memory_*"));
        });
    }

    #[test]
    fn pre_tool_use_surfaces_tool_triggered_coaching_rule() {
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            crate::coaching::apply_rule(
                "revfix",
                "Reviews ship with fixes",
                "Every finding needs a diff.",
                Some(crate::coaching::test_support::trigger(
                    vec!["mcp__flare__review".to_string()],
                    false,
                )),
            )
            .unwrap();

            let bodies = crate::coaching::rule_bodies_for_tool("mcp__flare__review");
            assert_eq!(bodies, vec!["Every finding needs a diff.".to_string()]);
            assert!(crate::coaching::rule_bodies_for_tool("mcp__flare__comment").is_empty());
        });
    }

    #[test]
    fn prompt_submit_surfaces_auto_match_coaching_rule() {
        use crate::paths::test_support::with_temp_home;
        with_temp_home(|| {
            crate::coaching::apply_rule(
                "revfix",
                "Reviews ship with fixes",
                "Every review finding needs a diff.",
                Some(crate::coaching::test_support::trigger(vec![], true)),
            )
            .unwrap();

            let bodies = crate::coaching::rule_bodies_for_prompt("please review this PR");
            assert_eq!(
                bodies,
                vec!["Every review finding needs a diff.".to_string()]
            );
            assert!(crate::coaching::rule_bodies_for_prompt("what's for lunch").is_empty());
        });
    }

    #[test]
    fn session_start_message_shows_pending_items_from_backend_db() {
        use crate::paths::test_support::with_temp_home;
        use agentflare_backend::item;

        with_temp_home(|| {
            let home = crate::paths::home();
            std::fs::create_dir_all(home.join(".agentflare")).unwrap();
            let db_path = home.join(".agentflare").join("backend.db");
            let conn = agentflare_backend::db::open_db(&db_path).unwrap();

            // Resolve the project through the SAME unconfigured
            // AgentflareMcp::default().resolve_project() path
            // session_start_message uses internally, so this test's items
            // land in the exact project the hook will actually look up --
            // proving the fix for the "picks an arbitrary project out of
            // every repo's items sharing this one backend.db" bug.
            let proj = crate::mcp_server::AgentflareMcp::default()
                .resolve_project(&conn)
                .unwrap();
            let sid = {
                let states = agentflare_backend::state::list_by_project(&conn, &proj.id).unwrap();
                states.iter().find(|s| s.is_default).unwrap().id.clone()
            };

            // Item assigned to the agent — should appear.
            item::create(
                &conn,
                item::CreateItem {
                    project_id: proj.id.clone(),
                    state_id: sid.clone(),
                    name: "Review PR #42".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: Some("claude-code".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();

            // Item assigned to a different agent — should NOT appear.
            item::create(
                &conn,
                item::CreateItem {
                    project_id: proj.id.clone(),
                    state_id: sid,
                    name: "Secret task".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: Some("other-agent".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();

            let msg = session_start_message("claude-code");
            assert!(
                msg.contains("Review PR #42"),
                "expected pending item in message, got:
{msg}"
            );
            assert!(
                msg.contains("Pending items assigned to you"),
                "expected pending section header in message, got:
{msg}"
            );
            assert!(
                !msg.contains("Secret task"),
                "other agent's item should not appear"
            );
        });
    }

    #[test]
    fn prompt_submit_expands_mentions_for_the_resolved_project() {
        use crate::paths::test_support::with_temp_home;
        use agentflare_backend::item;

        with_temp_home(|| {
            let home = crate::paths::home();
            std::fs::create_dir_all(home.join(".agentflare")).unwrap();
            let db_path = home.join(".agentflare").join("backend.db");
            let conn = agentflare_backend::db::open_db(&db_path).unwrap();

            let proj = crate::mcp_server::AgentflareMcp::default()
                .resolve_project(&conn)
                .unwrap();
            let sid = {
                let states = agentflare_backend::state::list_by_project(&conn, &proj.id).unwrap();
                states.iter().find(|s| s.is_default).unwrap().id.clone()
            };
            let created = item::create(
                &conn,
                item::CreateItem {
                    project_id: proj.id,
                    state_id: sid,
                    name: "Fix login timeout".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();

            let block = crate::mentions::expand(&format!("check @I:{}", created.id)).unwrap();
            assert!(block.contains("Fix login timeout"));

            assert!(crate::mentions::expand("no mentions here").is_none());
        });
    }
}
