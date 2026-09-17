//! Per-channel behavior policies (adopted from OpenFang's `openfang-types`).
//!
//! Policies are data, not code: hosts store them in config and evaluate them
//! through [`dm_allows`] / [`group_allows`], so adding a channel never means
//! adding an `if telegram` branch.

use serde::{Deserialize, Serialize};

/// DM (direct message) policy for a channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DmPolicy {
    /// Respond to all DMs.
    Respond,
    /// Only respond to DMs from authorized users (allowlist / pairing).
    #[default]
    AllowedOnly,
    /// Ignore all DMs.
    Ignore,
}

/// Group message policy for a channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupPolicy {
    /// Respond to all group messages.
    All,
    /// Only respond when explicitly addressed (@-mention, DM, reply).
    #[default]
    MentionOnly,
    /// Only respond to slash commands.
    CommandsOnly,
    /// Ignore all group messages.
    Ignore,
}

/// Output format hint for channel-specific message formatting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// Standard Markdown.
    #[default]
    Markdown,
    /// Telegram HTML subset.
    TelegramHtml,
    /// Slack mrkdwn format.
    SlackMrkdwn,
    /// Plain text (no formatting).
    PlainText,
}

/// Prefix style applied to outbound agent messages when several agents
/// share one channel, so readers can tell who authored a reply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrefixStyle {
    /// No prefix.
    #[default]
    Off,
    /// Plain bracketed name: `[agent-name] text`.
    Bracket,
    /// Bold bracketed name: `**[agent-name]** text`.
    BoldBracket,
}

/// Apply a prefix style to an outbound reply. [`PrefixStyle::Off`] returns
/// the text unchanged.
#[must_use]
pub fn apply_prefix(style: PrefixStyle, agent_name: &str, text: &str) -> String {
    match style {
        PrefixStyle::Off => text.to_string(),
        PrefixStyle::Bracket => format!("[{agent_name}] {text}"),
        PrefixStyle::BoldBracket => format!("**[{agent_name}]** {text}"),
    }
}

/// Whether an inbound DM may be answered under `policy`.
/// `sender_authorized` comes from the host's allowlist / pairing check.
#[must_use]
pub const fn dm_allows(policy: DmPolicy, sender_authorized: bool) -> bool {
    match policy {
        DmPolicy::Respond => true,
        DmPolicy::AllowedOnly => sender_authorized,
        DmPolicy::Ignore => false,
    }
}

/// Whether an inbound group message may be answered under `policy`.
#[must_use]
pub const fn group_allows(
    policy: GroupPolicy,
    explicitly_addressed: bool,
    is_command: bool,
) -> bool {
    match policy {
        GroupPolicy::All => true,
        GroupPolicy::MentionOnly => explicitly_addressed,
        GroupPolicy::CommandsOnly => is_command,
        GroupPolicy::Ignore => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dm_policy_truth_table() {
        assert!(dm_allows(DmPolicy::Respond, false));
        assert!(dm_allows(DmPolicy::AllowedOnly, true));
        assert!(!dm_allows(DmPolicy::AllowedOnly, false));
        assert!(!dm_allows(DmPolicy::Ignore, true));
    }

    #[test]
    fn group_policy_truth_table() {
        assert!(group_allows(GroupPolicy::All, false, false));
        assert!(group_allows(GroupPolicy::MentionOnly, true, false));
        assert!(!group_allows(GroupPolicy::MentionOnly, false, true));
        assert!(group_allows(GroupPolicy::CommandsOnly, false, true));
        assert!(!group_allows(GroupPolicy::CommandsOnly, true, false));
        assert!(!group_allows(GroupPolicy::Ignore, true, true));
    }

    #[test]
    fn prefix_styles() {
        assert_eq!(apply_prefix(PrefixStyle::Off, "a", "hi"), "hi");
        assert_eq!(apply_prefix(PrefixStyle::Bracket, "a", "hi"), "[a] hi");
        assert_eq!(
            apply_prefix(PrefixStyle::BoldBracket, "a", "hi"),
            "**[a]** hi"
        );
    }

    #[test]
    fn policies_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&DmPolicy::AllowedOnly).unwrap(),
            "\"allowed_only\""
        );
        assert_eq!(
            serde_json::to_string(&GroupPolicy::MentionOnly).unwrap(),
            "\"mention_only\""
        );
        assert_eq!(
            serde_json::to_string(&OutputFormat::TelegramHtml).unwrap(),
            "\"telegram_html\""
        );
    }
}
