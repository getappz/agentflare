//! Paged read of one session's turns and tool calls (the `recall` MCP action).

use serde::Serialize;
use serde_json::Value;

use crate::model::{Session, ToolCall, Turn};
use crate::store::InsightsStore;

pub const DEFAULT_TURN_LIMIT: usize = 20;
pub const DEFAULT_MAX_CHARS: usize = 12_000;

#[derive(Debug, Clone, Serialize)]
pub struct RecallPage {
    pub session: Session,
    pub turns: Vec<Turn>,
    pub tool_calls: Vec<ToolCall>,
    /// Seq to pass as `turn_start` for the next page; `None` at the end.
    pub next_turn_start: Option<u32>,
    /// True when `max_chars` deferred later turns or omitted anything on this page
    /// (not when `turn_limit` alone ended it).
    pub truncated: bool,
    /// Chars of turn and tool-call text cut to fit `max_chars`.
    pub omitted_chars: usize,
    /// Tool calls left off this page because the budget ran out.
    pub omitted_tool_calls: usize,
}

/// One page of a session, starting at `turn_start`. `None` for an unknown id.
/// Every page holds at least one turn so paging always makes progress.
pub fn recall_page(
    store: &InsightsStore,
    source: Option<&str>,
    session_id: &str,
    turn_start: u32,
    turn_limit: usize,
    max_chars: usize,
) -> anyhow::Result<Option<RecallPage>> {
    recall_page_scrubbed(
        store,
        source,
        session_id,
        turn_start,
        turn_limit,
        max_chars,
        &|_| {},
    )
}

/// [`recall_page`] with `scrub` applied before text is cut to `max_chars`, so a
/// redactor sees whole values and cannot be defeated by a secret cut in half.
pub fn recall_page_scrubbed(
    store: &InsightsStore,
    source: Option<&str>,
    session_id: &str,
    turn_start: u32,
    turn_limit: usize,
    max_chars: usize,
    scrub: &dyn Fn(&mut Value),
) -> anyhow::Result<Option<RecallPage>> {
    let Some(session) = store.get_session_by_provider_id(session_id)? else {
        return Ok(None);
    };
    if source.is_some_and(|s| s != session.source.as_str()) {
        return Ok(None);
    }
    let limit = turn_limit.max(1);
    // One extra turn tells us whether a next page exists.
    let rest = store.get_turns_page(&session.id, turn_start, limit.saturating_add(1))?;

    let (mut turns, mut chars, mut truncated) = (Vec::new(), 0usize, false);
    let mut next_turn_start = None;
    for t in rest {
        let len = t.user_text.as_deref().map_or(0, |s| s.chars().count())
            + t.assistant_text.as_deref().map_or(0, |s| s.chars().count());
        let over_chars = !turns.is_empty() && chars + len > max_chars;
        if turns.len() >= limit || over_chars {
            truncated = over_chars;
            next_turn_start = Some(t.seq);
            break;
        }
        chars += len;
        turns.push(t);
    }
    let mut tool_calls = match (turns.first(), turns.last()) {
        (Some(first), Some(last)) => {
            store.get_tool_calls_in_turns(&session.id, first.seq, last.seq)?
        }
        _ => Vec::new(),
    };
    let (omitted_chars, omitted_tool_calls) =
        bound_text(&mut turns, &mut tool_calls, max_chars, scrub);
    truncated |= omitted_chars > 0 || omitted_tool_calls > 0;
    Ok(Some(RecallPage {
        session,
        turns,
        tool_calls,
        next_turn_start,
        truncated,
        omitted_chars,
        omitted_tool_calls,
    }))
}

/// Scrub all page text, then spend one exact `max_chars` budget across it. Returns
/// (chars omitted, tool calls dropped once the budget ran out).
fn bound_text(
    turns: &mut [Turn],
    tool_calls: &mut Vec<ToolCall>,
    max_chars: usize,
    scrub: &dyn Fn(&mut Value),
) -> (usize, usize) {
    let mut left = max_chars;
    let mut omitted = 0;
    for t in turns.iter_mut() {
        for text in [&mut t.user_text, &mut t.assistant_text]
            .into_iter()
            .flatten()
        {
            omitted += scrub_and_clip(text, &mut left, scrub);
        }
    }
    let mut kept = 0;
    for c in tool_calls.iter_mut() {
        if left == 0 {
            break;
        }
        kept += 1;
        scrub(&mut c.input);
        let n = if c.input.is_null() {
            0
        } else {
            c.input.to_string().chars().count()
        };
        if n > left {
            c.input = Value::Null;
            omitted += n;
        } else {
            left -= n;
        }
        if let Some(output) = c.output.as_mut() {
            omitted += scrub_and_clip(output, &mut left, scrub);
        }
    }
    let dropped = tool_calls.len() - kept;
    tool_calls.truncate(kept);
    (omitted, dropped)
}

/// Returns how many chars were cut from `text`.
fn scrub_and_clip(text: &mut String, left: &mut usize, scrub: &dyn Fn(&mut Value)) -> usize {
    let mut value = Value::String(std::mem::take(text));
    scrub(&mut value);
    *text = match value {
        Value::String(s) => s,
        other => other.to_string(),
    };
    let n = text.chars().count();
    if n <= *left {
        *left -= n;
        return 0;
    }
    *text = text.chars().take(*left).collect();
    let cut = n - *left;
    *left = 0;
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use chrono::Utc;

    pub(crate) fn session(id: &str, turns: u32) -> Session {
        Session {
            id: id.into(),
            source: SessionSource::ClaudeCode,
            project: "demo".into(),
            project_path: None,
            title: None,
            model: None,
            status: SessionStatus::Completed,
            awaiting_reason: None,
            started_at: None,
            updated_at: Utc::now(),
            ended_at: None,
            duration_secs: None,
            tokens: TokenUsage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: None,
            cost_basis: CostBasis::Unknown,
            pricing_key: None,
            turn_count: turns,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: None,
        }
    }

    fn turn(sid: &str, seq: u32) -> Turn {
        Turn {
            id: format!("{sid}-t{seq}"),
            session_id: sid.into(),
            seq,
            user_text: Some(format!("u{seq}")),
            assistant_text: Some(format!("a{seq}")),
            started_at: None,
            ended_at: None,
            tokens: None,
            cost_usd: None,
        }
    }

    #[test]
    fn recall_paginates_deterministically() {
        let store = InsightsStore::open_in_memory().unwrap();
        store.upsert_session(&session("s1", 45)).unwrap();
        let ts: Vec<Turn> = (0..45).rev().map(|i| turn("s1", i)).collect();
        store.upsert_turns_batch(&ts).unwrap();

        let mut seen = Vec::new();
        let mut start = 0;
        let mut pages = 0;
        loop {
            let p = recall_page(&store, None, "s1", start, 20, 12_000)
                .unwrap()
                .unwrap();
            pages += 1;
            seen.extend(p.turns.iter().map(|t| t.seq));
            match p.next_turn_start {
                Some(n) => start = n,
                None => break,
            }
        }
        assert_eq!(pages, 3);
        assert_eq!(seen, (0..45).collect::<Vec<_>>());
        let first = recall_page(&store, None, "s1", 0, 20, 12_000)
            .unwrap()
            .unwrap();
        assert_eq!(first.turns.len(), 20);
        assert_eq!(first.next_turn_start, Some(20));
        assert!(!first.truncated);
        // tiny char budget truncates but still progresses
        let small = recall_page(&store, None, "s1", 0, 20, 1).unwrap().unwrap();
        assert_eq!(small.turns.len(), 1);
        assert!(small.truncated);
        assert_eq!(small.next_turn_start, Some(1));
    }

    #[test]
    fn recall_unknown_id_is_none() {
        let store = InsightsStore::open_in_memory().unwrap();
        assert!(recall_page(&store, None, "nope", 0, 20, 12_000)
            .unwrap()
            .is_none());
    }

    fn big_call(sid: &str, seq: u32, input: Value, output: &str) -> ToolCall {
        ToolCall {
            id: format!("{sid}-c{seq}"),
            session_id: sid.into(),
            turn_seq: seq,
            name: "Bash".into(),
            input,
            output: Some(output.into()),
            status: "completed".into(),
            duration_ms: None,
            created_at: None,
        }
    }

    #[test]
    fn tool_text_is_bounded_by_max_chars_and_paging_still_progresses() {
        let store = InsightsStore::open_in_memory().unwrap();
        store.upsert_session(&session("s1", 2)).unwrap();
        store
            .upsert_turns_batch(&[turn("s1", 0), turn("s1", 1)])
            .unwrap();
        let huge = "x".repeat(50_000);
        store
            .upsert_tool_call(&big_call("s1", 0, serde_json::json!({"cmd": huge}), &huge))
            .unwrap();
        let p = recall_page(&store, None, "s1", 0, 1, 1_000)
            .unwrap()
            .unwrap();
        let call = &p.tool_calls[0];
        assert!(p.truncated);
        assert!(call.input.is_null());
        assert_eq!(call.output.as_deref().unwrap().len(), 996);
        assert_eq!(p.omitted_chars, 50_010 + 49_004);
        assert_eq!(p.next_turn_start, Some(1));
    }

    #[test]
    fn many_tool_calls_cannot_exceed_a_small_budget() {
        let store = InsightsStore::open_in_memory().unwrap();
        store.upsert_session(&session("s1", 1)).unwrap();
        store.upsert_turns_batch(&[turn("s1", 0)]).unwrap();
        for i in 0..50 {
            let mut call = big_call("s1", 0, Value::Null, "abc");
            call.id = format!("c{i}");
            store.upsert_tool_call(&call).unwrap();
        }
        let p = recall_page(&store, None, "s1", 0, 1, 20).unwrap().unwrap();
        let turn_text = 4;
        let tool_text: usize = p
            .tool_calls
            .iter()
            .map(|c| c.output.as_deref().map_or(0, str::len))
            .sum();
        assert_eq!(turn_text + tool_text, 20);
        assert!(p.truncated);
        assert_eq!(p.tool_calls.len() + p.omitted_tool_calls, 50);
        assert!(p.omitted_tool_calls > 40);
    }

    #[test]
    fn scrub_runs_before_text_is_cut() {
        let store = InsightsStore::open_in_memory().unwrap();
        store.upsert_session(&session("s1", 1)).unwrap();
        let mut t = turn("s1", 0);
        t.user_text = Some("SECRETVALUE".into());
        t.assistant_text = None;
        store.upsert_turns_batch(&[t]).unwrap();
        let scrub = |v: &mut Value| {
            if let Value::String(s) = v {
                *s = s.replace("SECRETVALUE", "R");
            }
        };
        let p = recall_page_scrubbed(&store, None, "s1", 0, 1, 4, &scrub)
            .unwrap()
            .unwrap();
        assert_eq!(p.turns[0].user_text.as_deref(), Some("R"));
        assert!(!p.truncated);
    }
}
