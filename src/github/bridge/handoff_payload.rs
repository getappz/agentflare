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
//!
//! The payload is base64-encoded before embedding, not embedded as raw
//! JSON: `content`/`completed`/`remaining` are caller-supplied text with no
//! constraint against containing ` -->` or even a full fake
//! `<!-- agentflare:handoff:v1 ...` sequence, either of which would
//! otherwise let [`HandoffPayload::extract`]'s delimiter search terminate
//! early or lock onto the wrong occurrence. Base64's alphabet excludes `<`,
//! `!`, `-`, and space, so the encoded blob can never contain either
//! delimiter regardless of what the caller writes.

use base64::Engine as _;

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
    /// Appends this payload as a hidden, base64-encoded marker after `body`
    /// (the human-readable text shown on the issue). Always the LAST thing
    /// in the returned string -- [`Self::extract`] relies on that to find
    /// its own marker rather than an earlier one already present in `body`.
    pub fn embed(&self, body: &str) -> String {
        let json = serde_json::to_string(self).unwrap_or_default();
        let encoded = base64::engine::general_purpose::STANDARD.encode(json.as_bytes());
        format!("{body}\n\n{MARKER_PREFIX}{encoded}{MARKER_SUFFIX}")
    }

    /// Recovers a payload previously written by [`Self::embed`], if `body`
    /// ends with one. Tolerant of a missing or malformed marker (a
    /// hand-edited issue, or one predating this format) -- returns `None`
    /// rather than failing the caller.
    ///
    /// Anchored to the END of `body` on both sides: `strip_suffix` requires
    /// the marker to be the very last thing present (true of every marker
    /// this module writes), and `rfind` for the prefix then takes the LAST
    /// match before that suffix -- so an earlier marker-shaped string
    /// sitting in the human-visible text above it is not mistaken for the
    /// real one.
    pub fn extract(body: &str) -> Option<HandoffPayload> {
        let before_suffix = body.trim_end().strip_suffix(MARKER_SUFFIX)?;
        let start = before_suffix.rfind(MARKER_PREFIX)? + MARKER_PREFIX.len();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&before_suffix[start..])
            .ok()?;
        let json = String::from_utf8(decoded).ok()?;
        serde_json::from_str(&json).ok()
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
    fn embed_without_a_thread_id_round_trips_to_none() {
        let mut p = payload();
        p.thread_id = None;
        let body = p.embed("text");
        assert_eq!(HandoffPayload::extract(&body).unwrap().thread_id, None);
    }

    #[test]
    fn extract_returns_none_for_a_body_with_no_marker() {
        assert_eq!(HandoffPayload::extract("just some text"), None);
        assert_eq!(HandoffPayload::extract(""), None);
    }

    #[test]
    fn extract_tolerates_a_hand_edited_or_pre_existing_issue() {
        assert_eq!(
            HandoffPayload::extract("some text\n\n<!-- agentflare:handoff:v1 not-base64! -->"),
            None
        );
    }

    #[test]
    fn a_field_containing_the_html_close_delimiter_still_round_trips() {
        // A naive "search forward for ` -->`" extractor would stop at the
        // delimiter INSIDE this field and truncate the marker; base64
        // encoding means the literal sequence can't appear in the marker at
        // all, so this must round-trip exactly.
        let mut p = payload();
        p.content = "before --> after, and before-->after too".into();
        let body = p.embed("visible text");
        assert_eq!(HandoffPayload::extract(&body), Some(p));
    }

    #[test]
    fn a_field_containing_the_marker_prefix_still_round_trips() {
        let mut p = payload();
        p.content = MARKER_PREFIX.to_string();
        let body = p.embed("visible text");
        assert_eq!(HandoffPayload::extract(&body), Some(p));
    }

    #[test]
    fn a_valid_looking_marker_in_the_visible_body_is_not_mistaken_for_the_real_one() {
        // The visible body already contains something that parses as a
        // (different) marker -- extract must still recover the one `embed`
        // actually appended at the end, not this earlier one.
        let fake = HandoffPayload {
            key: "fake".into(),
            content: "fake".into(),
            completed: "fake".into(),
            remaining: "fake".into(),
            thread_id: None,
        };
        let visible = fake.embed("visible");
        let body = payload().embed(&visible);
        assert_eq!(HandoffPayload::extract(&body), Some(payload()));
    }
}
