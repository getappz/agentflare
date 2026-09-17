//! Normalized message types shared by every channel.
//!
//! Mirrors ZeroClaw's `ChannelMessage` / `SendMessage` split: inbound updates
//! are normalized once at the transport edge so the agent turn, router, and
//! realtime bus never touch platform JSON.

use serde::{Deserialize, Serialize};

/// Kind of an inbound attachment, without the payload itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentKind {
    Photo,
    Document,
    Audio,
    Video,
    Voice,
    Sticker,
    Other,
}

/// A normalized inbound attachment pointer (file id / URL + kind).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaAttachment {
    pub kind: AttachmentKind,
    pub file_id: String,
    pub caption: Option<String>,
}

impl MediaAttachment {
    #[must_use]
    pub fn new(kind: AttachmentKind, file_id: impl Into<String>) -> Self {
        Self {
            kind,
            file_id: file_id.into(),
            caption: None,
        }
    }
}

/// One normalized inbound message, regardless of originating platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// Platform-side message id (Telegram `message_id`, Slack `ts`, ...).
    pub id: String,
    /// Stable sender identity (numeric Telegram user id preferred).
    pub sender: String,
    /// Where replies go (Telegram `chat_id`, Slack channel, Discord channel).
    pub reply_target: String,
    /// Normalized text content (command prefix intact, e.g. `/status ...`).
    pub content: String,
    /// Channel name that produced this message (`telegram`, `slack`, ...).
    pub channel: String,
    /// Normalized media attached to the message.
    #[serde(default)]
    pub attachments: Vec<MediaAttachment>,
    /// Whether the bot was explicitly addressed (@-mention / DM / reply).
    #[serde(default)]
    pub explicitly_addressed: bool,
}

impl ChannelMessage {
    #[must_use]
    pub fn text(
        id: impl Into<String>,
        sender: impl Into<String>,
        reply_target: impl Into<String>,
        content: impl Into<String>,
        channel: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            sender: sender.into(),
            reply_target: reply_target.into(),
            content: content.into(),
            channel: channel.into(),
            attachments: Vec::new(),
            explicitly_addressed: false,
        }
    }
}

/// One normalized outbound message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendMessage {
    /// Text (or HTML, per channel) to deliver.
    pub content: String,
    /// Target id — maps back to [`ChannelMessage::reply_target`].
    pub recipient: String,
    /// Optional thread/topic to reply inside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_ts: Option<String>,
}

impl SendMessage {
    #[must_use]
    pub fn new(recipient: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            recipient: recipient.into(),
            thread_ts: None,
        }
    }

    #[must_use]
    pub fn threaded(mut self, thread_ts: impl Into<String>) -> Self {
        self.thread_ts = Some(thread_ts.into());
        self
    }
}

/// Realtime lifecycle events fanned out on the [`crate::ChatBus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelEvent {
    /// A normalized inbound message arrived.
    Inbound(ChannelMessage),
    /// An outbound reply was handed to the transport.
    Outbound(SendMessage),
    /// Remote party is typing / agent turn started (ephemeral UI hint).
    Typing { channel: String, target: String },
    /// A streaming draft was updated in place (message id + latest text).
    Draft {
        channel: String,
        target: String,
        message_id: String,
        text: String,
    },
    /// Handling for an inbound id fully settled (safe to confirm offset).
    Settled { channel: String, id: String },
}

/// Agent lifecycle phase for UX indicators (typing, reactions, drafts).
/// Mirrors OpenFang's `AgentPhase` so transports can share emoji/wording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    /// Message queued, waiting for the agent.
    Queued,
    /// Agent is calling the LLM.
    Thinking,
    /// Agent is executing a tool.
    ToolUse {
        /// Tool being executed (sanitized, see [`AgentPhase::tool_use`]).
        tool_name: String,
    },
    /// Agent is streaming tokens.
    Streaming,
    /// Agent finished successfully.
    Done,
    /// Agent encountered an error.
    Error,
}

impl AgentPhase {
    /// Build a [`AgentPhase::ToolUse`] with control chars stripped and the
    /// name truncated to 64 chars (it is rendered into chat UI).
    #[must_use]
    pub fn tool_use(name: &str) -> Self {
        let sanitized: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
        Self::ToolUse {
            tool_name: sanitized,
        }
    }
}

/// Reaction to show on a message for an agent phase (emoji-based).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleReaction {
    /// The agent phase this reaction represents.
    pub phase: AgentPhase,
    /// Channel-appropriate emoji.
    pub emoji: String,
    /// Whether to remove the previous phase reaction.
    pub remove_previous: bool,
}

impl LifecycleReaction {
    /// Reaction with the default emoji for `phase`.
    #[must_use]
    pub fn for_phase(phase: AgentPhase) -> Self {
        let emoji = default_phase_emoji(&phase).to_string();
        Self {
            phase,
            emoji,
            remove_previous: true,
        }
    }
}

/// Hardcoded emoji allowlist for lifecycle reactions (prevents arbitrary
/// emoji injection from tool names or model output).
pub const ALLOWED_REACTION_EMOJI: &[&str] = &[
    "🤔", // thinking
    "⚙️", // tool_use
    "✍️", // streaming
    "✅", // done
    "❌", // error
    "⏳", // queued
    "🔄", // processing
    "👀", // looking
];

/// Default emoji for an agent phase.
#[must_use]
pub const fn default_phase_emoji(phase: &AgentPhase) -> &'static str {
    match phase {
        AgentPhase::Queued => "⏳",
        AgentPhase::Thinking => "🤔",
        AgentPhase::ToolUse { .. } => "⚙️",
        AgentPhase::Streaming => "✍️",
        AgentPhase::Done => "✅",
        AgentPhase::Error => "❌",
    }
}

/// Health snapshot for one channel transport. Hosts expose it through
/// `doctor`-style checks; transports update it internally.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelStatus {
    /// Whether the transport is currently connected/running.
    pub connected: bool,
    /// Total inbound messages normalized since start.
    pub messages_received: u64,
    /// Total outbound messages sent since start.
    pub messages_sent: u64,
    /// Last error message, if any (sanitized — never a token or URL).
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_message_defaults() {
        let m = ChannelMessage::text("1", "42", "42", "hi", "telegram");
        assert!(m.attachments.is_empty());
        assert!(!m.explicitly_addressed);
    }

    #[test]
    fn send_message_threaded() {
        let m = SendMessage::new("42", "hi").threaded("7");
        assert_eq!(m.thread_ts.as_deref(), Some("7"));
    }

    #[test]
    fn event_round_trips_through_json() {
        let e = ChannelEvent::Inbound(ChannelMessage::text("1", "s", "t", "hi", "telegram"));
        let v = serde_json::to_string(&e).expect("serialize");
        let back: ChannelEvent = serde_json::from_str(&v).expect("deserialize");
        assert_eq!(e, back);
    }

    #[test]
    fn tool_use_sanitizes_name() {
        let phase = AgentPhase::tool_use("cargo test\u{0}\u{1f}extra");
        match phase {
            AgentPhase::ToolUse { tool_name } => {
                assert_eq!(tool_name, "cargo testextra");
            }
            _ => panic!("expected ToolUse"),
        }
        let long = "x".repeat(100);
        match AgentPhase::tool_use(&long) {
            AgentPhase::ToolUse { tool_name } => assert_eq!(tool_name.len(), 64),
            _ => panic!("expected ToolUse"),
        }
    }

    #[test]
    fn default_emoji_covers_every_phase() {
        for phase in [
            AgentPhase::Queued,
            AgentPhase::Thinking,
            AgentPhase::tool_use("t"),
            AgentPhase::Streaming,
            AgentPhase::Done,
            AgentPhase::Error,
        ] {
            let emoji = default_phase_emoji(&phase);
            assert!(
                ALLOWED_REACTION_EMOJI.contains(&emoji),
                "default emoji {emoji} must be allowlisted"
            );
            let reaction = LifecycleReaction::for_phase(phase);
            assert!(reaction.remove_previous);
        }
    }
}
