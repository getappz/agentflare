//! Telegram Bot API transport.
//!
//! Pure protocol helpers (URL/body builders, the 4096 splitter, update
//! parsing, offset math) are free functions so the existing supervisor flow
//! can migrate piece by piece. [`TelegramChannel`] implements [`Channel`] on
//! top of them; the binary does not use it yet (step 1 is behavior-free).
//!
//! Design notes (from ZeroClaw's `telegram.rs`, adapted to this codebase):
//! - Blocking `ureq` under `spawn_blocking`, matching the host's sync model.
//!   A native async HTTP backend can replace `post()` later without touching
//!   callers — error strings never embed the token-bearing URL (same rule as
//!   `channels::describe_send_error` in the binary).
//! - Approval `callback_query` updates are parsed ([`callback_of`]) but NOT
//!   forwarded by [`Channel::listen`]; the approval-card flow stays host-side
//!   until step 2 moves it over.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{
    AttachmentKind, Channel, ChannelError, ChannelMessage, ChannelResult, ChannelStatus,
    LifecycleReaction, MediaAttachment, SendMessage, TokenProvider,
};

/// Default Telegram Bot API root. Override for self-hosted Bot API servers.
pub const TELEGRAM_API_BASE: &str = "https://api.telegram.org";
/// Strict per-message character limit enforced server-side.
pub const TELEGRAM_MAX_MESSAGE_LENGTH: usize = 4096;
/// `getUpdates` long-poll hold time (no inbound port or webhook needed).
pub const TELEGRAM_IDLE_POLL_TIMEOUT_SECS: u64 = 30;
/// Backoff bounds for the poll loop (network errors, 409s).
pub const TELEGRAM_MIN_BACKOFF_SECS: u64 = 1;
pub const TELEGRAM_MAX_BACKOFF_SECS: u64 = 60;

/// Static channel name (`Channel::name`). Per-bot aliases (`telegram.home`)
/// live in the [`crate::ChannelRegistry`] keys, not here.
pub const TELEGRAM_CHANNEL_NAME: &str = "telegram";

/// Configuration for one Telegram bot instance.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    /// Registry alias (e.g. `telegram.home`). Used in error strings only.
    pub alias: String,
    /// Bot API root (default [`TELEGRAM_API_BASE`]).
    pub api_base_url: String,
    /// Long-poll hold per `getUpdates` call.
    pub poll_timeout_secs: u64,
    /// Max characters per outbound chunk (default 4096).
    pub chunk_limit: usize,
    /// Bot username without `@`, for group mention detection.
    pub mention_bot_username: Option<String>,
    /// First offset to poll from (host restores the persisted one here).
    pub initial_offset: i64,
    /// Commands advertised via `setMyCommands` at startup. Empty skips
    /// registration (hosts without a command surface leave this empty).
    pub bot_commands: Vec<BotCommand>,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            alias: "telegram".to_string(),
            api_base_url: TELEGRAM_API_BASE.to_string(),
            poll_timeout_secs: TELEGRAM_IDLE_POLL_TIMEOUT_SECS,
            chunk_limit: TELEGRAM_MAX_MESSAGE_LENGTH,
            mention_bot_username: None,
            initial_offset: 0,
            bot_commands: Vec::new(),
        }
    }
}

impl TelegramConfig {
    #[must_use]
    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = alias.into();
        self
    }
}

// ---------------------------------------------------------------------------
// Pure request builders (token passed explicitly, never logged).
// ---------------------------------------------------------------------------

/// `POST {base}/bot{token}/{method}`.
#[must_use]
pub fn method_url(api_base: &str, token: &str, method: &str) -> String {
    format!("{api_base}/bot{token}/{method}")
}

/// Convenience for the plain-text send path.
#[must_use]
pub fn send_message_url(api_base: &str, token: &str) -> String {
    method_url(api_base, token, "sendMessage")
}

/// Body for a plain-text `sendMessage`.
#[must_use]
pub fn send_body(target: &str, text: &str) -> serde_json::Value {
    serde_json::json!({ "chat_id": target, "text": text })
}

/// Body for an HTML `sendMessage` carrying one row of inline-keyboard
/// buttons (`(label, callback_data)` pairs). HTML needs only `&`/`<`/`>`
/// escaped, unlike the much larger MarkdownV2 escape set.
#[must_use]
pub fn card_body(target: &str, html_text: &str, buttons: &[(&str, &str)]) -> serde_json::Value {
    let row: Vec<serde_json::Value> = buttons
        .iter()
        .map(|(text, data)| serde_json::json!({ "text": text, "callback_data": data }))
        .collect();
    serde_json::json!({
        "chat_id": target,
        "text": html_text,
        "parse_mode": "HTML",
        "reply_markup": { "inline_keyboard": [row] },
    })
}

/// Body acknowledging a tapped button (toast on the button; required quickly
/// or Telegram shows the button as stuck/loading client-side).
#[must_use]
pub fn answer_callback_body(callback_query_id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({ "callback_query_id": callback_query_id, "text": text })
}

/// Body stripping the inline keyboard off an acted-on card so a stale
/// message cannot be tapped a second time.
#[must_use]
pub fn clear_markup_body(chat_id: &str, message_id: i64) -> serde_json::Value {
    serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "reply_markup": { "inline_keyboard": [] },
    })
}

/// Body for the streaming-draft edit path (`finalize_draft`).
#[must_use]
pub fn edit_text_body(chat_id: &str, message_id: i64, text: &str) -> serde_json::Value {
    serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": text,
    })
}

/// Body for one `getUpdates` call. Fetches `message` and `callback_query`
/// together: Telegram allows exactly one poller per token and advancing the
/// offset confirms every update below it regardless of filter.
#[must_use]
pub fn updates_body(offset: i64, timeout_secs: u64) -> serde_json::Value {
    serde_json::json!({
        "offset": offset,
        "timeout": timeout_secs,
        "allowed_updates": ["message", "callback_query"],
    })
}

/// One advertised bot command (`setMyCommands` entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotCommand {
    /// Lowercase `a-z0-9_` name (Telegram rejects anything else).
    pub command: String,
    /// Short description shown in the client command menu.
    pub description: String,
}

impl BotCommand {
    /// Sanitize to Telegram's command rules; returns `None` when nothing
    /// usable remains (caller skips it rather than failing the whole menu).
    #[must_use]
    pub fn new(command: &str, description: &str) -> Option<Self> {
        let command: String = command
            .trim_start_matches('/')
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if command.is_empty() || command.len() > 32 {
            return None;
        }
        let description = description.trim();
        let description = if description.is_empty() {
            command.clone()
        } else {
            description.chars().take(256).collect()
        };
        Some(Self {
            command,
            description,
        })
    }
}

/// Body advertising the command menu (`setMyCommands`, max 100 entries).
#[must_use]
pub fn set_commands_body(commands: &[BotCommand]) -> serde_json::Value {
    let list: Vec<serde_json::Value> = commands
        .iter()
        .take(100)
        .map(|c| serde_json::json!({ "command": c.command, "description": c.description }))
        .collect();
    serde_json::json!({ "commands": list })
}

/// Body clearing a stale webhook mapping so `getUpdates` polling does not
/// hit HTTP 409. `drop_pending_updates = false` keeps the server-side
/// backlog: hosts with a persisted offset (like ours) replay it instead of
/// silently losing messages — the deliberate difference from OpenFang,
/// which drops pending on restart.
#[must_use]
pub fn delete_webhook_body(drop_pending_updates: bool) -> serde_json::Value {
    serde_json::json!({ "drop_pending_updates": drop_pending_updates })
}

/// Body for a chat action (`typing`, `upload_photo`, ...).
#[must_use]
pub fn send_chat_action_body(target: &str, action: &str) -> serde_json::Value {
    serde_json::json!({ "chat_id": target, "action": action })
}

/// Body reacting to a message (`setMessageReaction`). Only emoji from
/// [`crate::ALLOWED_REACTION_EMOJI`] should reach this; the transport does
/// not re-validate.
#[must_use]
pub fn set_reaction_body(chat_id: &str, message_id: i64, emoji: &str) -> serde_json::Value {
    serde_json::json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "reaction": [{ "type": "emoji", "emoji": emoji }],
    })
}

/// Extract `parameters.retry_after` from a 429 response body, if present.
#[must_use]
pub fn retry_after_secs(body: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("parameters")?
        .get("retry_after")?
        .as_u64()
}

// ---------------------------------------------------------------------------
// Outbound chunking.
// ---------------------------------------------------------------------------

/// Split over-long text at line boundaries, preserving fenced code blocks:
/// a chunk broken inside a ``` fence is closed and the next chunk re-opens
/// it, so every chunk renders validly on its own. Every returned chunk is at
/// most `limit` characters (single over-long lines are hard-cut on char
/// boundaries with room reserved for the fence markers).
#[must_use]
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(64);
    // Room for a close + reopen marker pair around any hard-cut piece.
    let piece_limit = limit - 8;
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;
    // Content budget for the running chunk: reserve the close marker while
    // inside a fence so appending it can never push past `limit`.
    let push_piece =
        |piece: &str, current: &mut String, chunks: &mut Vec<String>, in_fence: &mut bool| {
            let toggles = piece.matches("```").count() % 2 == 1;
            let budget = if *in_fence { limit - 4 } else { limit };
            if current.len() + piece.len() > budget && !current.is_empty() {
                if *in_fence {
                    current.push_str("```\n");
                }
                chunks.push(std::mem::take(current));
                if *in_fence {
                    current.push_str("```\n");
                }
            }
            current.push_str(piece);
            if toggles {
                *in_fence = !*in_fence;
            }
        };
    for line in text.split_inclusive('\n') {
        // Hard-cut single over-long lines first so the accumulator below
        // only ever sees pieces that fit alongside the fence markers.
        let mut rest = line;
        while rest.len() > piece_limit {
            let mut end = piece_limit;
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            push_piece(&rest[..end], &mut current, &mut chunks, &mut in_fence);
            rest = &rest[end..];
        }
        push_piece(rest, &mut current, &mut chunks, &mut in_fence);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

// ---------------------------------------------------------------------------
// Inbound parsing.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct IncomingUser {
    pub id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IncomingChat {
    pub id: i64,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DocumentRef {
    pub file_id: String,
}

/// Subset of Telegram's `message` object needed for normalization.
/// Unknown fields are ignored so API additions do not break parsing.
#[derive(Debug, Clone, Deserialize)]
pub struct IncomingMessage {
    pub message_id: i64,
    pub chat: IncomingChat,
    pub from: Option<IncomingUser>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub photo: Option<Vec<PhotoSize>>,
    #[serde(default)]
    pub document: Option<DocumentRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallbackMessageRef {
    pub message_id: i64,
    pub chat: IncomingChat,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IncomingCallback {
    pub id: String,
    pub data: Option<String>,
    pub message: Option<CallbackMessageRef>,
}

/// Subset of a `getUpdates` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct RawUpdate {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<IncomingMessage>,
    #[serde(default)]
    pub callback_query: Option<IncomingCallback>,
}

/// Approval-flow payload for one tapped inline button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackInfo {
    pub callback_id: String,
    pub data: String,
    pub chat_id: String,
    pub message_id: Option<i64>,
}

/// Extract the tapped-button payload, if this update carries one.
#[must_use]
pub fn callback_of(update: &RawUpdate) -> Option<CallbackInfo> {
    let cb = update.callback_query.as_ref()?;
    let data = cb.data.clone()?;
    let m = cb.message.as_ref()?;
    let chat_id = m.chat.id.to_string();
    let message_id = Some(m.message_id);
    Some(CallbackInfo {
        callback_id: cb.id.clone(),
        data,
        chat_id,
        message_id,
    })
}

fn mentions_bot(text: &str, bot_username: Option<&str>) -> bool {
    match bot_username {
        Some(bot) if !bot.is_empty() => text.contains(&format!("@{bot}")),
        _ => false,
    }
}

/// Normalize one update into a [`ChannelMessage`]. Returns `None` for
/// non-message updates (callbacks) and for non-text content without a
/// caption (photos, stickers, ...) — the host skips those, as today.
#[must_use]
pub fn normalize_message(
    update: &RawUpdate,
    channel: &str,
    bot_username: Option<&str>,
) -> Option<ChannelMessage> {
    let m = update.message.as_ref()?;
    let content = m.text.clone().or_else(|| m.caption.clone())?;
    if content.trim().is_empty() {
        return None;
    }
    let sender = m
        .from
        .as_ref()
        .map(|u| u.id.to_string())
        .unwrap_or_else(|| m.chat.id.to_string());
    let is_private = m.chat.kind.as_deref() == Some("private");
    let mut msg = ChannelMessage::text(
        m.message_id.to_string(),
        sender,
        m.chat.id.to_string(),
        content.clone(),
        channel,
    );
    msg.explicitly_addressed = is_private || mentions_bot(&content, bot_username);
    if let Some(sizes) = &m.photo
        && let Some(largest) = sizes.last()
    {
        let mut a = MediaAttachment::new(AttachmentKind::Photo, largest.file_id.clone());
        a.caption = m.caption.clone();
        msg.attachments.push(a);
    }
    if let Some(doc) = &m.document {
        let mut a = MediaAttachment::new(AttachmentKind::Document, doc.file_id.clone());
        a.caption = m.caption.clone();
        msg.attachments.push(a);
    }
    Some(msg)
}

// ---------------------------------------------------------------------------
// Offset math (pure; mirrors the supervisor so it can adopt this later).
// ---------------------------------------------------------------------------

/// `getUpdates` offset convention: pass `last_update_id + 1`.
#[must_use]
pub fn next_offset(update_id: i64) -> i64 {
    update_id + 1
}

/// Highest offset safe to confirm: `ceiling` unless an in-flight update
/// sits below it, in which case stop just short so a crash replays it.
#[must_use]
pub fn safe_offset_to_persist(ceiling: i64, in_flight: &BTreeSet<i64>) -> i64 {
    in_flight
        .iter()
        .next()
        .copied()
        .unwrap_or(ceiling)
        .min(ceiling)
}

// ---------------------------------------------------------------------------
// Transport.
// ---------------------------------------------------------------------------

fn agent_for(poll_timeout_secs: u64) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(poll_timeout_secs + 30))
        .build()
}

/// Classify a `ureq` failure without ever rendering the request URL (which
/// embeds the live bot token for Telegram).
fn describe_ureq(alias: &str, err: &ureq::Error) -> String {
    let mut msg = format!("request to {alias} failed: {}", err.kind());
    if let ureq::Error::Transport(transport) = err {
        if let Some(detail) = transport.message() {
            msg.push_str(": ");
            msg.push_str(detail);
        }
        if let Some(source) = std::error::Error::source(transport) {
            msg.push_str(": ");
            msg.push_str(&source.to_string());
        }
    }
    msg
}

/// Runtime counters behind [`TelegramChannel::status`].
#[derive(Debug, Default)]
struct ChannelStats {
    connected: bool,
    received: u64,
    sent: u64,
    last_error: Option<String>,
}

/// One Telegram bot instance. Clone is cheap (shared agent + config).
#[derive(Clone)]
pub struct TelegramChannel {
    config: TelegramConfig,
    tokens: Arc<dyn TokenProvider>,
    agent: ureq::Agent,
    stats: Arc<Mutex<ChannelStats>>,
}

impl TelegramChannel {
    #[must_use]
    pub fn new(config: TelegramConfig, tokens: Arc<dyn TokenProvider>) -> Self {
        let agent = agent_for(config.poll_timeout_secs);
        Self {
            config,
            tokens,
            agent,
            stats: Arc::new(Mutex::new(ChannelStats::default())),
        }
    }

    #[must_use]
    pub fn config(&self) -> &TelegramConfig {
        &self.config
    }

    fn transport_err(&self, message: impl Into<String>) -> ChannelError {
        ChannelError::Transport {
            channel: self.config.alias.clone(),
            message: message.into(),
        }
    }

    fn token(&self) -> ChannelResult<String> {
        self.tokens.token_for(&self.config.alias).ok_or_else(|| {
            self.transport_err(format!(
                "no bot token for '{}' — configure it host-side first",
                self.config.alias
            ))
        })
    }

    fn record_error(&self, message: String) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.last_error = Some(message);
        }
    }

    fn post(&self, method: &str, body: &serde_json::Value) -> ChannelResult<serde_json::Value> {
        let token = self.token()?;
        let url = method_url(&self.config.api_base_url, &token, method);
        let resp = match self.agent.post(&url).send_json(body) {
            Ok(resp) => resp,
            Err(ureq::Error::Status(code, resp)) => {
                let body_txt = resp.into_string().unwrap_or_default();
                // 429 is backoff-and-retry, not fatal (honor `retry_after`
                // like OpenFang); 409 (stale polling session) is absorbed by
                // the listen loop's backoff below.
                if code == 429 {
                    let retry_after = retry_after_secs(&body_txt).unwrap_or(5);
                    self.record_error(format!("{method} rate limited, retry after {retry_after}s"));
                    return Err(ChannelError::RateLimited {
                        channel: self.config.alias.clone(),
                        retry_after_secs: retry_after,
                    });
                }
                let err = self.transport_err(format!("{method} HTTP {code}: {body_txt}"));
                self.record_error(err.to_string());
                return Err(err);
            }
            Err(e) => {
                let err = self.transport_err(describe_ureq(&self.config.alias, &e));
                self.record_error(err.to_string());
                return Err(err);
            }
        };
        resp.into_json()
            .map_err(|e| self.transport_err(format!("{method} response was not JSON: {e}")))
    }

    fn ensure_ok(&self, method: &str, value: &serde_json::Value) -> ChannelResult<()> {
        if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            Ok(())
        } else {
            let desc = value
                .get("description")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown error");
            Err(self.transport_err(format!("{method} rejected: {desc}")))
        }
    }

    fn send_blocking(&self, target: &str, text: &str) -> ChannelResult<()> {
        if text.trim().is_empty() {
            return Err(self.transport_err("refusing to send an empty message"));
        }
        let chunks = split_message(text, self.config.chunk_limit);
        for chunk in &chunks {
            let value = self.post("sendMessage", &send_body(target, chunk))?;
            self.ensure_ok("sendMessage", &value)?;
        }
        if let Ok(mut stats) = self.stats.lock() {
            stats.sent += chunks.len() as u64;
        }
        Ok(())
    }

    /// Resolve the bot identity (`getMe`), failing fast on a bad token.
    /// Shared by startup validation and host-side `doctor` checks.
    pub fn bot_identity_blocking(&self) -> ChannelResult<String> {
        let value = self.post("getMe", &serde_json::json!({}))?;
        self.ensure_ok("getMe", &value)?;
        value
            .get("result")
            .and_then(|r| r.get("username"))
            .and_then(serde_json::Value::as_str)
            .map(|u| format!("@{u}"))
            .ok_or_else(|| self.transport_err("getMe: missing result.username"))
    }

    /// Clear a stale webhook mapping left by a previous process.
    pub fn clear_webhook_blocking(&self, drop_pending_updates: bool) -> ChannelResult<()> {
        let value = self.post("deleteWebhook", &delete_webhook_body(drop_pending_updates))?;
        self.ensure_ok("deleteWebhook", &value)
    }

    /// Advertise the command menu. No-op when empty.
    pub fn register_commands_blocking(&self, commands: &[BotCommand]) -> ChannelResult<()> {
        if commands.is_empty() {
            return Ok(());
        }
        let value = self.post("setMyCommands", &set_commands_body(commands))?;
        self.ensure_ok("setMyCommands", &value)
    }

    /// One-time startup: validate the token (fail fast), mark the transport
    /// connected, clear a stale webhook mapping, and advertise the command
    /// menu. Webhook/command failures are non-fatal (visible in status);
    /// only a bad token aborts before polling starts.
    fn startup(&self) -> ChannelResult<()> {
        self.bot_identity_blocking()?;
        if let Ok(mut stats) = self.stats.lock() {
            stats.connected = true;
        }
        let _ = self.clear_webhook_blocking(false);
        if !self.config.bot_commands.is_empty() {
            let _ = self.register_commands_blocking(&self.config.bot_commands.clone());
        }
        Ok(())
    }

    /// One blocking `getUpdates` round. Hosts with their own runtime call
    /// this directly; [`Channel::listen`] loops it under `spawn_blocking`.
    pub fn poll_once(&self, offset: i64) -> ChannelResult<Vec<RawUpdate>> {
        let value = self.post(
            "getUpdates",
            &updates_body(offset, self.config.poll_timeout_secs),
        )?;
        if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(self.transport_err("getUpdates reported ok=false"));
        }
        let items = value
            .get("result")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        // Skip single malformed updates rather than wedging the whole poll.
        let updates: Vec<RawUpdate> = items
            .into_iter()
            .filter_map(|item| serde_json::from_value(item).ok())
            .collect();
        if let Ok(mut stats) = self.stats.lock() {
            stats.received += updates.len() as u64;
        }
        Ok(updates)
    }

    async fn blocking<F, T>(&self, f: F) -> ChannelResult<T>
    where
        F: FnOnce(Self) -> ChannelResult<T> + Send + 'static,
        T: Send + 'static,
    {
        let this = self.clone();
        tokio::task::spawn_blocking(|| f(this))
            .await
            .map_err(|e| ChannelError::Transport {
                channel: self.config.alias.clone(),
                message: format!("telegram task failed: {e}"),
            })?
    }
}

#[async_trait::async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &'static str {
        TELEGRAM_CHANNEL_NAME
    }

    async fn send(&self, message: SendMessage) -> ChannelResult<()> {
        self.blocking(move |this| this.send_blocking(&message.recipient, &message.content))
            .await
    }

    async fn finalize_draft(
        &self,
        target: &str,
        message_id: &str,
        text: &str,
    ) -> ChannelResult<()> {
        let message_id: i64 = message_id
            .parse()
            .map_err(|_| self.transport_err(format!("invalid draft message id: {message_id}")))?;
        let target = target.to_string();
        let text = text.to_string();
        self.blocking(move |this| {
            let value = this.post(
                "editMessageText",
                &edit_text_body(&target, message_id, &text),
            )?;
            this.ensure_ok("editMessageText", &value)
        })
        .await
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> ChannelResult<()> {
        // Fail fast on a bad token and clear any stale webhook mapping left
        // by a previous process before the first poll (avoids 409s).
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.startup())
            .await
            .map_err(|e| ChannelError::Transport {
                channel: self.config.alias.clone(),
                message: format!("telegram startup task failed: {e}"),
            })??;

        let mut offset = self.config.initial_offset;
        let bot_username = self.config.mention_bot_username.clone();
        let mut backoff_secs = TELEGRAM_MIN_BACKOFF_SECS;
        loop {
            let this = self.clone();
            let updates = tokio::task::spawn_blocking(move || this.poll_once(offset))
                .await
                .map_err(|e| ChannelError::Transport {
                    channel: self.config.alias.clone(),
                    message: format!("telegram poll task failed: {e}"),
                })?;
            match updates {
                Ok(updates) => {
                    backoff_secs = TELEGRAM_MIN_BACKOFF_SECS;
                    for update in &updates {
                        offset = offset.max(next_offset(update.update_id));
                        if let Some(msg) = normalize_message(
                            update,
                            TELEGRAM_CHANNEL_NAME,
                            bot_username.as_deref(),
                        ) {
                            tx.send(msg).await.map_err(|_| ChannelError::Transport {
                                channel: self.config.alias.clone(),
                                message: "channel listener gone".to_string(),
                            })?;
                        }
                    }
                }
                // 429/409-style transients: sleep and retry with backoff
                // rather than aborting the listener.
                Err(ChannelError::RateLimited {
                    retry_after_secs, ..
                }) => {
                    tokio::time::sleep(Duration::from_secs(retry_after_secs.max(1))).await;
                }
                Err(_) => {
                    let wait = backoff_secs;
                    backoff_secs = (backoff_secs * 2).min(TELEGRAM_MAX_BACKOFF_SECS);
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                }
            }
        }
    }

    async fn send_typing(&self, target: &str) -> ChannelResult<()> {
        let target = target.to_string();
        self.blocking(move |this| {
            let value = this.post("sendChatAction", &send_chat_action_body(&target, "typing"))?;
            this.ensure_ok("sendChatAction", &value)
        })
        .await
    }

    async fn send_reaction(
        &self,
        target: &str,
        message_id: &str,
        reaction: LifecycleReaction,
    ) -> ChannelResult<()> {
        if !crate::ALLOWED_REACTION_EMOJI.contains(&reaction.emoji.as_str()) {
            return Err(self.transport_err("reaction emoji is not allowlisted"));
        }
        let message_id: i64 = message_id.parse().map_err(|_| {
            self.transport_err(format!("invalid reaction message id: {message_id}"))
        })?;
        let target = target.to_string();
        let emoji = reaction.emoji.clone();
        self.blocking(move |this| {
            let value = this.post(
                "setMessageReaction",
                &set_reaction_body(&target, message_id, &emoji),
            )?;
            this.ensure_ok("setMessageReaction", &value)
        })
        .await
    }

    fn status(&self) -> ChannelStatus {
        match self.stats.lock() {
            Ok(stats) => ChannelStatus {
                connected: stats.connected,
                messages_received: stats.received,
                messages_sent: stats.sent,
                last_error: stats.last_error.clone(),
            },
            Err(_) => ChannelStatus::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct MapTokens(Mutex<HashMap<String, String>>);

    impl TokenProvider for MapTokens {
        fn token_for(&self, channel: &str) -> Option<String> {
            self.0.lock().expect("lock").get(channel).cloned()
        }
    }

    fn channel_with_token(alias: &str) -> TelegramChannel {
        let mut map = HashMap::new();
        map.insert(alias.to_string(), "TOKEN".to_string());
        TelegramChannel::new(
            TelegramConfig::default().with_alias(alias),
            Arc::new(MapTokens(Mutex::new(map))),
        )
    }

    #[test]
    fn urls_keep_token_in_path_with_no_auth_header() {
        let url = send_message_url("https://api.telegram.org", "TOKEN");
        assert_eq!(url, "https://api.telegram.org/botTOKEN/sendMessage");
        assert_eq!(
            method_url("https://example.invalid", "T", "getMe"),
            "https://example.invalid/botT/getMe"
        );
    }

    #[test]
    fn card_body_is_html_with_one_button_row() {
        let body = card_body("42", "<b>hi</b>", &[("Approve", "approve:x")]);
        assert_eq!(body["parse_mode"], "HTML");
        assert_eq!(
            body["reply_markup"]["inline_keyboard"][0][0]["callback_data"],
            "approve:x"
        );
    }

    #[test]
    fn updates_body_fetches_messages_and_callbacks() {
        let body = updates_body(7, 30);
        assert_eq!(body["offset"], 7);
        assert_eq!(body["timeout"], 30);
        let allowed = body["allowed_updates"].as_array().expect("array");
        assert!(allowed.iter().any(|v| v == "message"));
        assert!(allowed.iter().any(|v| v == "callback_query"));
    }

    #[test]
    fn short_text_is_not_split() {
        assert_eq!(split_message("hi", 4096), vec!["hi".to_string()]);
    }

    #[test]
    fn long_text_splits_on_lines_within_limit() {
        let line = "a".repeat(100);
        let text = (0..50).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
        let chunks = split_message(&text, 1000);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= 1000));
        assert_eq!(chunks.join(""), text);
    }

    #[test]
    fn fence_is_closed_and_reopened_across_chunks() {
        let body = "x".repeat(900);
        let text = format!("before\n```\n{body}\n```\nafter");
        let chunks = split_message(&text, 500);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert_eq!(
                chunk.matches("```").count() % 2,
                0,
                "unbalanced fence in {chunk:?}"
            );
        }
    }

    fn update_fixture() -> serde_json::Value {
        serde_json::json!([
            {
                "update_id": 100,
                "message": {
                    "message_id": 7,
                    "from": { "id": 42 },
                    "chat": { "id": 42, "type": "private" },
                    "text": "hello"
                }
            },
            {
                "update_id": 101,
                "message": {
                    "message_id": 8,
                    "from": { "id": 43 },
                    "chat": { "id": -5, "type": "group" },
                    "text": "hi all"
                }
            },
            {
                "update_id": 102,
                "message": {
                    "message_id": 9,
                    "from": { "id": 43 },
                    "chat": { "id": -5, "type": "group" },
                    "text": "@mybot status",
                    "entities": [{ "type": "mention", "offset": 0, "length": 6 }]
                }
            },
            {
                "update_id": 103,
                "callback_query": {
                    "id": "cb1",
                    "data": "approve:o/r#1",
                    "message": {
                        "message_id": 9,
                        "chat": { "id": 42, "type": "private" }
                    }
                }
            }
        ])
    }

    fn parse_all() -> Vec<RawUpdate> {
        update_fixture()
            .as_array()
            .expect("array")
            .iter()
            .cloned()
            .map(|v| serde_json::from_value(v).expect("parse"))
            .collect()
    }

    #[test]
    fn normalize_private_group_and_mention() {
        let updates = parse_all();
        let dm = normalize_message(&updates[0], "telegram", None).expect("dm");
        assert!(dm.explicitly_addressed);
        assert_eq!(dm.sender, "42");

        let group = normalize_message(&updates[1], "telegram", Some("mybot")).expect("group");
        assert!(!group.explicitly_addressed);

        let mention = normalize_message(&updates[2], "telegram", Some("mybot")).expect("mention");
        assert!(mention.explicitly_addressed);
    }

    #[test]
    fn callbacks_parse_but_do_not_normalize_to_messages() {
        let updates = parse_all();
        assert!(normalize_message(&updates[3], "telegram", None).is_none());
        let cb = callback_of(&updates[3]).expect("callback");
        assert_eq!(cb.callback_id, "cb1");
        assert_eq!(cb.data, "approve:o/r#1");
        assert_eq!(cb.chat_id, "42");
        assert_eq!(cb.message_id, Some(9));
    }

    #[test]
    fn offsets_follow_telegram_convention() {
        assert_eq!(next_offset(100), 101);
        let empty = BTreeSet::new();
        assert_eq!(safe_offset_to_persist(101, &empty), 101);
        let in_flight: BTreeSet<i64> = [102, 105].into_iter().collect();
        assert_eq!(safe_offset_to_persist(106, &in_flight), 102);
    }

    #[test]
    fn missing_token_errors_clearly_without_leaking() {
        let ch = TelegramChannel::new(
            TelegramConfig::default(),
            Arc::new(MapTokens(Mutex::new(HashMap::new()))),
        );
        let err = ch.send_blocking("42", "hi").expect_err("no token");
        let msg = err.to_string();
        assert!(msg.contains("telegram"));
        assert!(!msg.contains("TOKEN"));
    }

    #[test]
    fn channel_reports_static_name_for_registry_keys() {
        let ch = channel_with_token("telegram.home");
        assert_eq!(ch.name(), "telegram");
        assert_eq!(ch.config().alias, "telegram.home");
    }

    #[test]
    fn bot_commands_sanitize_to_telegram_rules() {
        let cmd = BotCommand::new("/Status", "Show project standup").expect("valid");
        assert_eq!(cmd.command, "status");
        assert!(BotCommand::new("!!!", "x").is_none());
        assert!(BotCommand::new("", "x").is_none());
        assert!(BotCommand::new(&"a".repeat(33), "x").is_none());
        // Empty description falls back to the command name.
        let cmd = BotCommand::new("help", "  ").expect("valid");
        assert_eq!(cmd.description, "help");
    }

    #[test]
    fn set_commands_body_caps_at_one_hundred() {
        let commands: Vec<BotCommand> = (0..105)
            .map(|i| BotCommand::new(&format!("cmd{i}"), "d").expect("valid"))
            .collect();
        let body = set_commands_body(&commands);
        assert_eq!(body["commands"].as_array().expect("array").len(), 100);
        assert_eq!(set_commands_body(&[])["commands"], serde_json::json!([]));
    }

    #[test]
    fn webhook_and_action_bodies() {
        assert_eq!(
            delete_webhook_body(false),
            serde_json::json!({ "drop_pending_updates": false })
        );
        let action = send_chat_action_body("42", "typing");
        assert_eq!(action["chat_id"], "42");
        assert_eq!(action["action"], "typing");
        let reaction = set_reaction_body("42", 7, "👀");
        assert_eq!(reaction["message_id"], 7);
        assert_eq!(reaction["reaction"][0]["emoji"], "👀");
        assert_eq!(reaction["reaction"][0]["type"], "emoji");
    }

    #[test]
    fn retry_after_parses_telegram_429_shape() {
        let body = r#"{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":7}}"#;
        assert_eq!(retry_after_secs(body), Some(7));
        assert_eq!(retry_after_secs(r#"{"ok":true}"#), None);
        assert_eq!(retry_after_secs("not json"), None);
    }

    #[test]
    fn fresh_channel_status_is_disconnected_with_zero_counters() {
        use crate::Channel as _;
        let ch = channel_with_token("telegram");
        let status = ch.status();
        assert!(!status.connected);
        assert_eq!(status.messages_received, 0);
        assert_eq!(status.messages_sent, 0);
        assert!(status.last_error.is_none());
    }
}
