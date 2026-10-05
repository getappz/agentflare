//! Best-effort detection of the model the host agent is currently running, from
//! what a hook already receives. Nothing here is authoritative: every layer can
//! be absent, so callers must treat `None` as "unknown", never as a default.
//!
//! Layers, first hit wins:
//! 1. a `model` field in the hook payload (Claude Code `SessionStart`, Cursor, ...);
//! 2. the latest assistant reply recorded in the transcript (`transcript_path`),
//!    which follows `/model` switches mid-session;
//! 3. Claude Code only: `ANTHROPIC_MODEL`, then the user settings `model`
//!    (often an alias such as `sonnet`, not a full id).
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Only the end of a transcript is read: it can grow to many megabytes.
const TAIL_BYTES: u64 = 256 * 1024;

pub fn detect(agent: &str, hook_input: &str) -> Option<String> {
    let payload: Value = serde_json::from_str(hook_input).ok()?;
    from_payload(&payload)
        .or_else(|| {
            let path = payload.get("transcript_path")?.as_str()?;
            from_transcript_tail(&read_tail(Path::new(path))?)
        })
        .or_else(|| agent.contains("claude").then(from_claude_config).flatten())
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn from_payload(payload: &Value) -> Option<String> {
    non_empty(payload.get("model")?.as_str()?)
}

/// Newest assistant entry that names a real model. `<synthetic>` is Claude
/// Code's marker for locally generated messages, not a model.
fn from_transcript_tail(tail: &str) -> Option<String> {
    tail.lines().rev().find_map(|line| {
        let entry: Value = serde_json::from_str(line).ok()?;
        let message = entry.get("message");
        let is_assistant = entry.get("type").and_then(Value::as_str) == Some("assistant")
            || message.and_then(|m| m.get("role")).and_then(Value::as_str) == Some("assistant");
        if !is_assistant {
            return None;
        }
        let model = message
            .and_then(|m| m.get("model"))
            .or_else(|| entry.get("model"))?
            .as_str()?;
        (!model.starts_with('<'))
            .then(|| non_empty(model))
            .flatten()
    })
}

fn read_tail(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let start = file.metadata().ok()?.len().saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    // Starting mid-file lands inside a line: drop the partial first line.
    Some(match (start > 0).then(|| text.find('\n')).flatten() {
        Some(i) => text[i + 1..].to_string(),
        None => text.into_owned(),
    })
}

fn from_claude_config() -> Option<String> {
    std::env::var("ANTHROPIC_MODEL")
        .ok()
        .and_then(|m| non_empty(&m))
        .or_else(|| {
            let settings = dirs::home_dir()?.join(".claude").join("settings.json");
            from_settings_text(&std::fs::read_to_string(settings).ok()?)
        })
}

fn from_settings_text(text: &str) -> Option<String> {
    non_empty(
        serde_json::from_str::<Value>(text)
            .ok()?
            .get("model")?
            .as_str()?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn assistant(model: &str) -> String {
        format!(r#"{{"type":"assistant","message":{{"role":"assistant","model":"{model}"}}}}"#)
    }

    #[test]
    fn payload_model_wins_over_everything() {
        let input = r#"{"session_id":"s","model":"claude-opus-5","transcript_path":"/nope"}"#;
        assert_eq!(
            detect("claude-code", input).as_deref(),
            Some("claude-opus-5")
        );
    }

    #[test]
    fn transcript_tail_gives_the_latest_assistant_model_following_switches() {
        let tail = [
            assistant("claude-sonnet-5"),
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.to_string(),
            assistant("claude-opus-5"),
            r#"{"type":"user","message":{"role":"user","content":"again"}}"#.to_string(),
        ]
        .join("\n");
        assert_eq!(
            from_transcript_tail(&tail).as_deref(),
            Some("claude-opus-5")
        );
    }

    #[test]
    fn synthetic_blank_and_garbage_entries_are_skipped() {
        let tail = [
            assistant("claude-haiku-4-5"),
            assistant("<synthetic>"),
            assistant("  "),
            "not json at all".to_string(),
        ]
        .join("\n");
        assert_eq!(
            from_transcript_tail(&tail).as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(from_transcript_tail("{}\n\n"), None);
    }

    #[test]
    fn reads_only_the_tail_of_a_large_transcript_and_drops_the_partial_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "{}", assistant("claude-sonnet-5")).unwrap();
        // pad well past TAIL_BYTES so the first (older) entry is outside the window
        let filler = r#"{"type":"user","message":{"role":"user","content":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}}"#;
        for _ in 0..(TAIL_BYTES as usize / filler.len() + 50) {
            writeln!(f, "{filler}").unwrap();
        }
        writeln!(f, "{}", assistant("claude-opus-5")).unwrap();
        drop(f);
        let tail = read_tail(&path).unwrap();
        assert!(tail.len() as u64 <= TAIL_BYTES);
        assert!(!tail.contains("claude-sonnet-5"));
        assert_eq!(
            from_transcript_tail(&tail).as_deref(),
            Some("claude-opus-5")
        );
        assert!(read_tail(&dir.path().join("missing.jsonl")).is_none());
    }

    #[test]
    fn detect_uses_the_transcript_when_the_payload_has_no_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, format!("{}\n", assistant("claude-sonnet-5"))).unwrap();
        let input = serde_json::json!({"session_id": "s", "transcript_path": path}).to_string();
        // a non-claude agent name proves layer 2 answered, not the config fallback
        assert_eq!(
            detect("opencode", &input).as_deref(),
            Some("claude-sonnet-5")
        );
    }

    #[test]
    fn unknown_for_other_agents_without_any_signal() {
        assert_eq!(detect("codex", r#"{"session_id":"s"}"#), None);
        assert_eq!(detect("codex", "not json"), None);
    }

    #[test]
    fn settings_model_may_be_an_alias() {
        assert_eq!(
            from_settings_text(r#"{"model":"sonnet"}"#).as_deref(),
            Some("sonnet")
        );
        assert_eq!(from_settings_text(r#"{"model":""}"#), None);
        assert_eq!(from_settings_text("{}"), None);
    }
}
