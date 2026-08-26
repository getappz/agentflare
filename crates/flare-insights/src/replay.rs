use crate::model::{ToolCall, Turn};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReplayTurn {
    pub turn: Turn,
    pub tool_calls: Vec<ToolCall>,
    pub subagent_ids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionReplay {
    pub session_id: String,
    pub turns: Vec<ReplayTurn>,
}

impl SessionReplay {
    pub fn from_parts(session_id: impl Into<String>, mut turns: Vec<Turn>, tool_calls: Vec<ToolCall>) -> Self {
        turns.sort_by_key(|t| t.seq);
        let mut by_turn: std::collections::HashMap<u32, Vec<ToolCall>> = std::collections::HashMap::new();
        for tc in tool_calls {
            by_turn.entry(tc.turn_seq).or_default().push(tc);
        }
        let turns = turns.into_iter().map(|t| {
            let tcs = by_turn.remove(&t.seq).unwrap_or_default();
            ReplayTurn { turn: t, tool_calls: tcs, subagent_ids: vec![] }
        }).collect();
        Self { session_id: session_id.into(), turns }
    }

    pub fn total_tokens(&self) -> u64 {
        self.turns.iter().filter_map(|r| r.turn.tokens.as_ref().map(|t| t.total())).sum()
    }
}
