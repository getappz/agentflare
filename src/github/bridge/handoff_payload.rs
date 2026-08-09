//! Embeds/recovers the structured handoff payload (`content`/`completed`/
//! `remaining`/`thread_id`) and an idempotency key onto/from a GitHub issue
//! body published via `handoff`'s `recipient="github"` path
//! (`mcp_server::handoff::handoff_to_bridge_queue`), read back by the bridge
//! importer (`tick::record_claim`) when it turns a claimed issue into a
//! local item, and looked up again by `handoff_to_bridge_queue` itself
//! before publishing to avoid a duplicate on retry.
//!
//! Kept as a single hidden HTML comment appended after the human-readable
//! body, so the visible issue text stays exactly what the caller wrote --
//! same rendering trick `bridge::marker` uses for claim state, but a
//! separate format: this is a one-shot descriptive payload, not the
//! append-only claim/heartbeat state machine `marker` models.

const MARKER_PREFIX: &str = "<!-- agentflare:handoff:v1 ";
const MARKER_SUFFIX: &str = " -->";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HandoffPayload {
    /// Dedup key for idempotent publication: the handoff's `thread_id` when
    /// given, else its `name`. Two publishes with the same key are the same
    /// logical handoff -- a retry after a timeout, not a second one.
    pub key: String,
    pub content: String,
    pub completed: String,
    pub remaining: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

impl HandoffPayload {
    /// Appends this payload as a hidden marker after `body` (the
    /// human-readable text shown on the issue).
    pub fn embed(&self, body: &str) -> String {
        format!(
            "{body}\n\n{MARKER_PREFIX}{}{MARKER_SUFFIX}",
            serde_json::to_string(self).unwrap_or_default()
        )
    }

    /// Recovers a payload previously written by [`Self::embed`], if `body`
    /// contains one. Tolerant of a missing or malformed marker (a hand-edited
    /// issue, or one predating this format) -- returns `None` rather than
    /// failing the caller.
    pub fn extract(body: &str) -> Option<HandoffPayload> {
        let start = body.find(MARKER_PREFIX)? + MARKER_PREFIX.len();
        let end = start + body[start..].find(MARKER_SUFFIX)?;
        serde_json::from_str(&body[start..end]).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> HandoffPayload {
        HandoffPayload {
            key: "k".into(),
            content: "c".into(),
            completed: "done so far".into(),
            remaining: "left to do".into(),
            thread_id: Some("t".into()),
        }
    }

    #[test]
    fn embed_then_extract_round_trips() {
        let body = payload().embed("visible text");
        assert!(body.starts_with("visible text"));
        assert_eq!(HandoffPayload::extract(&body), Some(payload()));
    }

    #[test]
    fn embed_without_a_thread_id_omits_it_rather_than_embedding_null() {
        let mut p = payload();
        p.thread_id = None;
        let body = p.embed("text");
        assert!(!body.contains("thread_id"));
        assert_eq!(HandoffPayload::extract(&body).unwrap().thread_id, None);
    }

    #[test]
    fn extract_returns_none_for_a_body_with_no_marker() {
        assert_eq!(HandoffPayload::extract("just some text"), None);
    }

    #[test]
    fn extract_tolerates_a_hand_edited_or_pre_existing_issue() {
        assert_eq!(
            HandoffPayload::extract("some text\n\n<!-- agentflare:handoff:v1 not json -->"),
            None
        );
        assert_eq!(HandoffPayload::extract(""), None);
    }
}
