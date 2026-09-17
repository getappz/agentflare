//! # flare-channels
//!
//! Modular channel abstraction for the agentflare platform: the [`Channel`]
//! trait, normalized [`ChannelMessage`] / [`SendMessage`] types, a named
//! [`ChannelRegistry`], and the realtime [`ChatBus`] event hub.
//!
//! Step 1 ships the Telegram transport (`telegram`) alongside the contracts,
//! but the binary does not use it yet — behavior-free until the supervisor
//! adopts it piece by piece. Transports implement [`Channel`]
//! against these types; hosts (`agentflare` binary, dashboard, CLI) consume
//! the registry and bus without importing any platform JSON.

pub mod bus;
pub mod policy;
pub mod registry;
pub mod telegram;
pub mod traits;
pub mod types;

pub use bus::{ChatBus, DEFAULT_BUS_CAPACITY};
pub use policy::{
    DmPolicy, GroupPolicy, OutputFormat, PrefixStyle, apply_prefix, dm_allows, group_allows,
};
pub use registry::{ChannelHandle, ChannelRegistry};
pub use telegram::{
    BotCommand, CallbackInfo, IncomingMessage, RawUpdate, TELEGRAM_API_BASE, TELEGRAM_CHANNEL_NAME,
    TELEGRAM_IDLE_POLL_TIMEOUT_SECS, TELEGRAM_MAX_BACKOFF_SECS, TELEGRAM_MAX_MESSAGE_LENGTH,
    TELEGRAM_MIN_BACKOFF_SECS, TelegramChannel, TelegramConfig, answer_callback_body, callback_of,
    card_body, clear_markup_body, delete_webhook_body, edit_text_body, method_url, next_offset,
    normalize_message, retry_after_secs, safe_offset_to_persist, send_body, send_chat_action_body,
    send_message_url, set_commands_body, set_reaction_body, split_message, updates_body,
};
pub use traits::{
    AgentOutcome, AgentRunner, Channel, ChannelError, ChannelResult, SessionStore, TokenProvider,
};
pub use types::{
    ALLOWED_REACTION_EMOJI, AgentPhase, AttachmentKind, ChannelEvent, ChannelMessage,
    ChannelStatus, LifecycleReaction, MediaAttachment, SendMessage, default_phase_emoji,
};
