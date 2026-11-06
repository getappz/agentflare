//! Paged read of one session's turns and tool calls (the `recall` MCP action).

use serde::Serialize;

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
    /// True when the page was cut short by `max_chars` (not by `turn_limit`).
    pub truncated: bool,
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
    let Some(session) = store.get_session_by_provider_id(session_id)? else {
        return Ok(None);
    };
    if source.is_some_and(|s| s != session.source.as_str()) {
        return Ok(None);
    }
    let mut rest: Vec<Turn> = store
        .get_turns(&session.id)?
        .into_iter()
        .filter(|t| t.seq >= turn_start)
        .collect();
    rest.sort_by_key(|t| t.seq);

    let (mut turns, mut chars, mut truncated) = (Vec::new(), 0usize, false);
    let mut iter = rest.into_iter();
    let mut next_turn_start = None;
    for t in iter.by_ref() {
        let len = t.user_text.as_deref().map_or(0, |s| s.chars().count())
            + t.assistant_text.as_deref().map_or(0, |s| s.chars().count());
        let over_chars = !turns.is_empty() && chars + len > max_chars;
        if turns.len() >= turn_limit.max(1) || over_chars {
            truncated = over_chars;
            next_turn_start = Some(t.seq);
            break;
        }
        chars += len;
        turns.push(t);
    }
    let seqs: std::collections::HashSet<u32> = turns.iter().map(|t| t.seq).collect();
    let tool_calls = store
        .get_tool_calls(&session.id)?
        .into_iter()
        .filter(|c| seqs.contains(&c.turn_seq))
        .collect();
    Ok(Some(RecallPage {
        session,
        turns,
        tool_calls,
        next_turn_start,
        truncated,
    }))
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
}
