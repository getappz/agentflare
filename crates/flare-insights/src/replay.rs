use crate::model::{FileEvent, ToolCall, Turn};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReplayTurn {
    pub turn: Turn,
    pub tool_calls: Vec<ToolCall>,
    pub file_events: Vec<FileEvent>,
    pub subagent_ids: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionReplay {
    pub session_id: String,
    pub turns: Vec<ReplayTurn>,
    pub total_tokens: u64,
    pub total_files: usize,
}

impl SessionReplay {
    /// DRY: build from DB rows, grouping by turn seq
    pub fn from_parts(
        session_id: impl Into<String>,
        mut turns: Vec<Turn>,
        tool_calls: Vec<ToolCall>,
        file_events: Vec<FileEvent>,
    ) -> Self {
        turns.sort_by_key(|t| t.seq);
        let mut by_turn_tools: std::collections::HashMap<u32, Vec<ToolCall>> =
            std::collections::HashMap::new();
        for tc in tool_calls {
            by_turn_tools.entry(tc.turn_seq).or_default().push(tc);
        }
        let mut by_turn_files: std::collections::HashMap<u32, Vec<FileEvent>> =
            std::collections::HashMap::new();
        // file_events don't have turn_seq, so assign by time proximity: for now, attach to all turns
        // Simple DRY: distribute by turn seq if file event's at is between turn start/end
        // For now, attach all to first turn if no better mapping
        let session_id_str = session_id.into();
        let total_tokens = turns
            .iter()
            .filter_map(|r| r.tokens.as_ref().map(|t| t.total()))
            .sum();
        let total_files = file_events.len();

        let replay_turns = turns
            .into_iter()
            .map(|t| {
                let seq = t.seq;
                let tcs = by_turn_tools.remove(&seq).unwrap_or_default();
                // naive: files whose at is within turn window, else attach to nearest
                let fes = if seq == 1 {
                    file_events.clone()
                } else {
                    vec![]
                };
                ReplayTurn {
                    turn: t,
                    tool_calls: tcs,
                    file_events: fes,
                    subagent_ids: vec![],
                }
            })
            .collect();

        Self {
            session_id: session_id_str,
            turns: replay_turns,
            total_tokens,
            total_files,
        }
    }

    pub fn timeline(&self) -> Vec<TimelineEvent> {
        let mut events = Vec::new();
        for rt in &self.turns {
            if let Some(user) = &rt.turn.user_text {
                events.push(TimelineEvent {
                    seq: rt.turn.seq,
                    kind: "user".into(),
                    detail: user.chars().take(200).collect(),
                    at: rt.turn.started_at,
                });
            }
            for tc in &rt.tool_calls {
                events.push(TimelineEvent {
                    seq: rt.turn.seq,
                    kind: format!("tool:{}", tc.name),
                    detail: tc
                        .input
                        .get("file_path")
                        .or_else(|| tc.input.get("path"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(&tc.name)
                        .to_string(),
                    at: tc.created_at,
                });
            }
            for fe in &rt.file_events {
                events.push(TimelineEvent {
                    seq: rt.turn.seq,
                    kind: format!("file:{}", fe.kind),
                    detail: fe.path.clone(),
                    at: Some(fe.at),
                });
            }
            if let Some(assistant) = &rt.turn.assistant_text {
                events.push(TimelineEvent {
                    seq: rt.turn.seq,
                    kind: "assistant".into(),
                    detail: assistant.chars().take(200).collect(),
                    at: rt.turn.ended_at,
                });
            }
        }
        events.sort_by(|a, b| a.at.cmp(&b.at));
        events
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TimelineEvent {
    pub seq: u32,
    pub kind: String,
    pub detail: String,
    pub at: Option<chrono::DateTime<chrono::Utc>>,
}
