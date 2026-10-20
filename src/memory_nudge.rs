//! Event-triggered memory recall (port of potpie's zero-token nudge model).
//! Hooks call `recall_for` on pre-edit / test-failure; matching observations
//! are ranked by hybrid search, filtered against a per-session injected-key
//! ledger, budgeted to `TOP_K`, and emitted as a compact source-ref-first
//! block. Fail-safe: any error or empty result yields `None` (inject nothing).
use rusqlite::Connection;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::memory::observations::Observation;

const TOP_K: usize = 3;
const SNIPPET_CHARS: usize = 240;
/// Search tokens are ANDed, so a failure query is one key line, a few words.
const QUERY_WORDS: usize = 8;
/// Sessions kept in the ledger file, and keys kept per session.
const MAX_SESSIONS: usize = 50;
const MAX_KEYS: usize = 500;

pub(crate) const STOP_INSTRUCTION: &str = "agentflare: before stopping, record durable learnings from this session (decisions, gotchas, fixes) with the memory remember tool; skip if there are none.";

#[derive(Clone, Copy)]
pub(crate) enum Event {
    PreEdit,
    TestFailed,
}

type Ledger = HashMap<String, (String, Vec<String>)>; // session -> (ts, keys)

fn ledger_path() -> PathBuf {
    crate::state::state_dir().join("memory-nudge-ledger.json")
}

fn load() -> Ledger {
    std::fs::read_to_string(ledger_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// True only when the ledger reached disk, so a caller never emits (and
/// thereby "claims") keys the ledger failed to record.
fn save(map: &Ledger) -> bool {
    let path = ledger_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_string(map) else {
        return false;
    };
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
        return true;
    }
    let _ = std::fs::remove_file(&tmp);
    false
}

/// Atomically (under the shared pace lock) drops keys already injected in
/// `session`, records the survivors, and returns them. `None` on lock failure
/// so a broken ledger injects nothing rather than repeating itself.
fn claim_fresh(session: &str, keys: Vec<String>, limit: usize) -> Option<Vec<String>> {
    let _lock = crate::nudge_pace::PaceLock::acquire().ok()?;
    let mut map = load();
    let now = chrono::Utc::now().to_rfc3339();
    let entry = map.entry(session.to_string()).or_default();
    // Only the first `limit` unseen keys are claimed: a key is recorded iff it
    // is actually emitted, so lower-ranked hits can surface on a later call.
    let fresh: Vec<String> = keys
        .into_iter()
        .filter(|k| !entry.1.contains(k))
        .take(limit)
        .collect();
    entry.1.extend(fresh.iter().cloned());
    if entry.1.len() > MAX_KEYS {
        let drop = entry.1.len() - MAX_KEYS;
        entry.1.drain(..drop);
    }
    entry.0 = now;
    if map.len() > MAX_SESSIONS {
        let mut by_age: Vec<_> = map.iter().map(|(k, v)| (v.0.clone(), k.clone())).collect();
        by_age.sort();
        for (_, k) in by_age.into_iter().take(map.len() - MAX_SESSIONS) {
            map.remove(&k);
        }
    }
    save(&map).then_some(fresh)
}

/// Ledger session key: agent-scoped, since agents can share a raw
/// `session_id` (same keying as the runtime session tracking).
fn ledger_key(agent: &str, session: &str) -> String {
    crate::messages::identity::hook_key(agent, session)
}

/// True the first time it is called for `session` (Stop instruction gate).
pub(crate) fn first_time(agent: &str, session: &str, key: &str) -> bool {
    claim_fresh(&ledger_key(agent, session), vec![key.to_string()], 1)
        .is_some_and(|f| !f.is_empty())
}

fn clip(s: &str, n: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= n {
        flat
    } else {
        format!("{}…", flat.chars().take(n).collect::<String>())
    }
}

/// The line most likely to name the failure, trimmed to `QUERY_WORDS` words.
fn failure_key_line(text: &str) -> String {
    let lines = || text.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = lines()
        .find(|l| {
            let l = l.to_lowercase();
            l.contains("error") || l.contains("failed") || l.contains("panicked")
        })
        .or_else(|| lines().next())
        .unwrap_or("");
    line.split_whitespace()
        .take(QUERY_WORDS)
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_block(event: Event, subject: &str, obs: &[&Observation]) -> String {
    let head = match event {
        Event::PreEdit => format!("agentflare memory — prior notes relevant to {subject}:"),
        Event::TestFailed => "agentflare memory — prior occurrences of this failure:".to_string(),
    };
    let mut out = head;
    for o in obs {
        out.push_str(&format!(
            "\n- [obs#{} {}] {}: {}",
            o.id,
            o.r#type,
            clip(&o.title, 80),
            clip(&o.content, SNIPPET_CHARS)
        ));
    }
    out
}

fn recall_with(
    conn: &Connection,
    event: Event,
    session: &str,
    project: &str,
    subject: &str,
    query: &str,
) -> Option<String> {
    let query = match event {
        Event::PreEdit => std::path::Path::new(query)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        Event::TestFailed => failure_key_line(query),
    };
    if query.is_empty() {
        return None;
    }
    let hits = crate::memory::search::search_hybrid(
        conn,
        &query,
        None,
        None,
        TOP_K * 3,
        crate::memory::engine::embed_query,
    )
    .ok()?;
    // Scope policy: this project's observations plus unscoped (global) ones;
    // never another project's. Filtered here because the search's own project
    // filter would drop the global ones.
    let hits: Vec<Observation> = hits
        .into_iter()
        .filter(|o| o.project.as_deref().is_none_or(|p| p == project))
        .collect();
    if hits.is_empty() {
        return None;
    }
    let keys: Vec<String> = hits.iter().map(|o| format!("obs:{}", o.id)).collect();
    let fresh = claim_fresh(session, keys, TOP_K)?;
    let picked: Vec<&Observation> = hits
        .iter()
        .filter(|o| fresh.contains(&format!("obs:{}", o.id)))
        .collect();
    (!picked.is_empty()).then(|| format_block(event, subject, &picked))
}

/// Recall for a hook event. Never panics, never errors; `None` = stay silent.
pub(crate) fn recall_for(
    event: Event,
    agent: &str,
    session: &str,
    subject: &str,
    query: &str,
) -> Option<String> {
    if session.is_empty() || !crate::memory::store::brain_db_path().exists() {
        return None;
    }
    std::panic::catch_unwind(|| {
        let conn = crate::memory::store::open().ok()?;
        let project = crate::mcp_server::AgentflareMcp::resolve_project_name();
        recall_with(
            &conn,
            event,
            &ledger_key(agent, session),
            &project,
            subject,
            query,
        )
    })
    .ok()
    .flatten()
}

/// Appends prior-failure recall to the PostToolUseFailure decision `out`
/// (or creates one); unchanged when nothing fresh is recalled.
pub(crate) fn with_failure_recall(
    agent: &str,
    input: &str,
    failure_text: &str,
    out: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(input).unwrap_or_default();
    let interrupt = v["is_interrupt"].as_bool().unwrap_or(false);
    let block = crate::hook_messages::parse_session(input)
        .session_id
        .filter(|_| !interrupt)
        .and_then(|sid| recall_for(Event::TestFailed, agent, &sid, "", failure_text));
    let Some(block) = block else {
        return out;
    };
    let prev = out
        .as_ref()
        .and_then(|o| o["hookSpecificOutput"]["additionalContext"].as_str());
    let ctx = prev.map_or_else(|| block.clone(), |p| format!("{p}\n\n{block}"));
    let event = if agent == "codex" {
        "PostToolUse"
    } else {
        "PostToolUseFailure"
    };
    Some(serde_json::json!({"hookSpecificOutput": {
        "hookEventName": event,
        "additionalContext": ctx,
    }}))
}

/// Recall for a mutating tool call's target file.
pub(crate) fn pre_edit_nudge(
    agent: &str,
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    session: &str,
) -> Option<String> {
    if !crate::hook_redirect::MUTATING_TOOLS.contains(&tool_name) {
        return None;
    }
    let path = edit_path(tool_input)?;
    recall_for(Event::PreEdit, agent, session, &path, &path)
}

/// File path a mutating tool call targets.
pub(crate) fn edit_path(tool_input: Option<&serde_json::Value>) -> Option<String> {
    let ti = tool_input?;
    ["file_path", "path", "notebook_path"]
        .iter()
        .find_map(|k| ti.get(k)?.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn pre_edit_injects_each_fact_once_and_failure_recall_works() {
        with_temp_home(|| {
            let conn = crate::memory::store::open().unwrap();
            crate::memory::observations::save(
                &conn,
                None,
                "gotcha",
                "parser.rs quirk",
                "parser.rs panics on empty input; guard first",
                None,
                None,
                None,
                None,
            )
            .unwrap();
            crate::memory::observations::save(
                &conn,
                None,
                "bug",
                "borrow checker failure",
                "error[E0502] cannot borrow as mutable fixed by cloning",
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let first = recall_for(
                Event::PreEdit,
                "claude",
                "s1",
                "src/parser.rs",
                "src/parser.rs parser",
            );
            assert!(
                first.as_deref().is_some_and(|b| b.contains("obs#")),
                "{first:?}"
            );
            assert!(
                recall_for(
                    Event::PreEdit,
                    "claude",
                    "s1",
                    "src/parser.rs",
                    "src/parser.rs parser"
                )
                .is_none()
            );
            let f = recall_for(
                Event::TestFailed,
                "claude",
                "s1",
                "",
                "error[E0502] cannot borrow as mutable",
            );
            assert!(f.is_some_and(|b| b.contains("E0502")));
        });
    }

    #[test]
    fn silent_when_memory_empty_or_session_missing() {
        with_temp_home(|| {
            assert!(recall_for(Event::PreEdit, "claude", "s1", "f", "anything").is_none());
            let _ = crate::memory::store::open().unwrap();
            assert!(recall_for(Event::PreEdit, "claude", "s1", "f", "anything").is_none());
            assert!(recall_for(Event::PreEdit, "claude", "", "f", "anything").is_none());
        });
    }

    #[test]
    fn stop_instruction_fires_once_per_session() {
        with_temp_home(|| {
            assert!(first_time("claude", "s1", "stop"));
            assert!(!first_time("claude", "s1", "stop"));
            assert!(first_time("claude", "s2", "stop"));
            // Agents sharing a raw session id keep separate ledgers.
            assert!(first_time("codex", "s1", "stop"));
        });
    }

    #[test]
    fn recall_skips_other_projects_but_keeps_unscoped() {
        with_temp_home(|| {
            let conn = crate::memory::store::open().unwrap();
            for (project, title) in [(Some("other"), "gizmo other"), (None, "gizmo global")] {
                crate::memory::observations::save(
                    &conn,
                    None,
                    "gotcha",
                    title,
                    "gizmo quirk",
                    None,
                    project,
                    None,
                    None,
                )
                .unwrap();
            }
            let b = recall_with(&conn, Event::PreEdit, "k", "mine", "gizmo", "src/gizmo").unwrap();
            assert!(
                b.contains("gizmo global") && !b.contains("gizmo other"),
                "{b}"
            );
        });
    }

    #[test]
    fn unwritable_ledger_claims_nothing() {
        with_temp_home(|| {
            std::fs::create_dir_all(ledger_path()).unwrap(); // rename onto a dir fails
            assert!(claim_fresh("k", vec!["a".into()], 1).is_none());
            assert!(!first_time("claude", "s1", "stop"));
        });
    }

    #[test]
    fn overflow_hits_surface_on_later_calls_and_bad_ledger_fails_safe() {
        with_temp_home(|| {
            let conn = crate::memory::store::open().unwrap();
            for i in 0..5 {
                crate::memory::observations::save(
                    &conn,
                    None,
                    "gotcha",
                    &format!("widget note {i}"),
                    &format!("widget quirk number {i}"),
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
            }
            let mut seen = std::collections::HashSet::new();
            for _ in 0..3 {
                if let Some(b) = recall_for(Event::PreEdit, "claude", "s1", "widget", "src/widget")
                {
                    for l in b.lines().filter(|l| l.starts_with("- [obs#")) {
                        assert!(seen.insert(l.to_string()), "repeated: {l}");
                    }
                }
            }
            assert_eq!(seen.len(), 5);
            // Corrupt ledger: load() falls back to empty; never panics/errs.
            std::fs::write(ledger_path(), "{not json").unwrap();
            let _ = recall_for(Event::PreEdit, "claude", "s2", "widget", "src/widget");
            // Unusable DB path: silent.
            assert!(recall_for(Event::TestFailed, "claude", "", "", "error x").is_none());
        });
    }
}
