//! Outbound channels: send a plain-text message out to a chat platform
//! (Telegram / Slack / Discord). This is the "outward" half of the channels
//! effort — a one-shot blocking POST that fits agentflare's sync/`ureq` model,
//! callable by an agent mid-run (MCP tool) or from the CLI. Bot tokens live in
//! the encrypted `gateway_secrets` store; the inbound daemon (flared) reuses
//! this same path to send its replies.
//!
//! Each platform renders `text` with whatever presentation it actually
//! supports, rather than a flat string everywhere:
//! - Telegram: `POST {base}/bot{token}/sendMessage`  body `{chat_id, text}` (token in URL).
//!   Card sends (approval gates) additionally set `parse_mode: HTML` and an
//!   inline keyboard -- see [`build_telegram_card_request`].
//! - Slack:    `POST slack.com/api/chat.postMessage`  body `{channel, text, blocks}`
//!   (Authorization: Bearer) -- `blocks` is a single Block Kit section so the
//!   message renders as a card; `text` stays a plain fallback (Slack's own
//!   notification/accessibility copy for clients that don't render blocks).
//! - Discord:  `POST discord.com/api/v10/channels/{id}/messages`  body `{embeds}`
//!   (Authorization: Bot) -- one embed carrying `text` as its description.
//!
//! Each platform needs a bot token stored under [`Platform::secret_name`] via
//! `agentflare vault set <secret_name>` (value piped over stdin) before
//! [`send_message`] will work — it errors out by name when the secret is
//! missing rather than failing silently. For getting a bot token/chat id per
//! platform and wiring up the supervisor's human-in-loop pings, see the
//! "Channel notifications" guide in `docs-site/src/content/docs/guides.md`.

use serde_json::{Value, json};

/// A supported outbound chat platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Telegram,
    Slack,
    Discord,
}

impl Platform {
    /// Parse a `--to` value (case-insensitive). `None` for unknown platforms.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "telegram" => Some(Self::Telegram),
            "slack" => Some(Self::Slack),
            "discord" => Some(Self::Discord),
            _ => None,
        }
    }

    /// The `gateway_secrets` entry holding this platform's bot token.
    #[must_use]
    pub fn secret_name(self) -> &'static str {
        match self {
            Self::Telegram => "telegram_bot_token",
            Self::Slack => "slack_bot_token",
            Self::Discord => "discord_bot_token",
        }
    }
}

/// A ready-to-send outbound HTTP request: where to POST, an optional
/// `Authorization` header value, and the JSON body.
pub struct OutboundRequest {
    pub url: String,
    pub auth: Option<String>,
    pub body: Value,
}

/// Build the platform-specific send request, rendered with whatever card
/// presentation that platform supports rather than a flat string.
#[must_use]
pub fn build_request(platform: Platform, target: &str, text: &str, token: &str) -> OutboundRequest {
    match platform {
        // Token goes in the URL path; no auth header.
        Platform::Telegram => OutboundRequest {
            url: flare_channels::send_message_url(flare_channels::TELEGRAM_API_BASE, token),
            auth: None,
            body: flare_channels::send_body(target, text),
        },
        // `blocks` is one Block Kit section so Slack renders this as a card
        // rather than a bare line; `text` stays alongside it as the
        // required fallback Slack uses for push notifications and clients
        // that don't render blocks. No interactive elements (buttons) yet --
        // that needs a Slack interactivity endpoint to receive the tap,
        // which nothing in this codebase serves today (unlike Telegram's
        // `getUpdates` poller).
        Platform::Slack => OutboundRequest {
            url: "https://slack.com/api/chat.postMessage".to_string(),
            auth: Some(format!("Bearer {token}")),
            body: json!({
                "channel": target,
                "text": text,
                "blocks": [
                    { "type": "section", "text": { "type": "mrkdwn", "text": text } }
                ],
            }),
        },
        // Discord uses the literal `Bot ` auth prefix (not `Bearer`), and
        // renders an embed as a card rather than plain message content.
        Platform::Discord => OutboundRequest {
            url: format!("https://discord.com/api/v10/channels/{target}/messages"),
            auth: Some(format!("Bot {token}")),
            body: json!({ "embeds": [{ "description": text }] }),
        },
    }
}

/// Decide whether a send succeeded from the HTTP status and response body.
/// Telegram/Discord are status-only; Slack returns HTTP 200 with `{"ok":false}`
/// on failure, so its body must be inspected.
pub fn interpret_response(platform: Platform, status: u16, body: &str) -> Result<(), String> {
    let ok_status = (200..300).contains(&status);
    match platform {
        Platform::Slack => {
            if !ok_status {
                return Err(format!("slack HTTP {status}: {body}"));
            }
            let parsed: Value = serde_json::from_str(body)
                .map_err(|e| format!("slack response was not JSON: {e}"))?;
            if parsed.get("ok").and_then(Value::as_bool) == Some(true) {
                Ok(())
            } else {
                let reason = parsed
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                Err(format!("slack rejected the message: {reason}"))
            }
        }
        Platform::Telegram | Platform::Discord => {
            if ok_status {
                Ok(())
            } else {
                Err(format!("HTTP {status}: {body}"))
            }
        }
    }
}

/// A shared `ureq` agent with explicit connect/read timeouts so a stalled or
/// silent platform can't hang the caller indefinitely. `ureq` 2.x defaults to
/// a 30s connect timeout but leaves the read/write timeout unset, so we build
/// our own agent instead of using the bare `ureq::post` free function.
fn http_agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
    })
}

/// Describe a transport-level send failure (DNS, connection refused, TLS,
/// timeout — anything short of getting an HTTP status back).
///
/// This must NOT use `ureq::Error`'s own `Display` impl (`{err}`) or
/// `Transport::url()`: `ureq`'s `Display` for both `Error` and `Transport`
/// unconditionally prepends the request URL (see `ureq`'s `error.rs`), and
/// for Telegram that URL embeds the live bot token
/// (`https://api.telegram.org/bot{token}/sendMessage`). This error string can
/// end up in CLI stderr or MCP client logs, so it's built instead from the
/// safe, URL-free pieces `ureq` exposes: the error's `kind()` classification,
/// its optional higher-level `message()`, and the underlying `source()` (a
/// plain `std::io::Error`/TLS error with no knowledge of the request URL).
/// `platform`'s `Debug` output (e.g. "Telegram") stands in for the URL.
fn describe_send_error(platform: Platform, err: &ureq::Error) -> String {
    let mut msg = format!("request to {platform:?} failed: {}", err.kind());
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

/// Execute a built request over blocking HTTP and interpret the outcome.
fn send(platform: Platform, req: &OutboundRequest) -> Result<(), String> {
    let mut r = http_agent().post(&req.url);
    if let Some(auth) = &req.auth {
        r = r.set("Authorization", auth);
    }
    // ureq returns non-2xx as `Err(Status(..))`; capture status+body from both
    // arms so `interpret_response` (e.g. Slack's `ok` field) sees the payload.
    let (status, body) = match r.send_json(&req.body) {
        Ok(resp) => (resp.status(), resp.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, resp)) => (code, resp.into_string().unwrap_or_default()),
        Err(e) => return Err(describe_send_error(platform, &e)),
    };
    interpret_response(platform, status, &body)
}

/// Resolve the platform's bot token from the encrypted `gateway_secrets` store
/// and send `text` to `target`. The one entry point CLI and MCP both call.
pub fn send_message(platform: Platform, target: &str, text: &str) -> Result<(), String> {
    let name = platform.secret_name();
    let token = crate::vault::get_secret(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "no {name} configured — store the bot token with 'agentflare vault set {name}' first"
            )
        })?;
    let req = build_request(platform, target, text, &token);
    send(platform, &req)
}

/// Look up the Telegram bot token -- shared by every Telegram-specific
/// helper below (cards, updates, callback acks) so they don't each repeat
/// [`send_message`]'s inline vault lookup.
fn telegram_token() -> Result<zeroize::Zeroizing<String>, String> {
    let name = Platform::Telegram.secret_name();
    crate::vault::get_secret(name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!(
                "no {name} configured — store the bot token with 'agentflare vault set {name}' first"
            )
        })
}

/// Call a no-argument (or query-string-only) Telegram Bot API GET method
/// and return its parsed body. Shared by [`telegram_bot_identity`] and
/// [`telegram_chat_identity`] so both get the same token-safe error
/// handling `get_telegram_updates_filtered` established (never let
/// `ureq::Error`'s own `Display` leak the token-bearing URL).
fn telegram_get(method: &str, query: &str) -> Result<Value, String> {
    let token = telegram_token()?;
    let url = format!("https://api.telegram.org/bot{}/{method}{query}", *token);
    let resp = match http_agent().get(&url).call() {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, resp)) => {
            return Err(format!(
                "telegram {method} HTTP {code}: {}",
                resp.into_string().unwrap_or_default()
            ));
        }
        Err(e) => return Err(describe_send_error(Platform::Telegram, &e)),
    };
    resp.into_json()
        .map_err(|e| format!("telegram {method} response was not JSON: {e}"))
}

/// The bot identity the configured `telegram_bot_token` actually resolves
/// to (`getMe`), for `doctor`'s channel-health check. Exists because the
/// chat channel can go permanently silent -- no error anywhere, `getUpdates`
/// just keeps returning an empty backlog -- when the vault's token is stale
/// or belongs to a different bot than whichever one a user is actually
/// messaging in Telegram; printing `@bot_username` lets that be caught by
/// eyeballing it against the real conversation instead of a long manual
/// trace. Read-only, no message is sent.
pub fn telegram_bot_identity() -> Result<String, String> {
    let parsed = telegram_get("getMe", "")?;
    let username = parsed
        .get("result")
        .and_then(|r| r.get("username"))
        .and_then(Value::as_str)
        .ok_or_else(|| "telegram getMe: missing result.username".to_string())?;
    Ok(format!("@{username}"))
}

/// The person/chat Telegram associates with `chat_id` (`getChat`) --
/// pairs with [`telegram_bot_identity`] so `doctor` can show BOTH ends of
/// the configured conversation. `telegram_notify_chat_id` can point at the
/// wrong person (stale, copy-pasted from an example, a former teammate's
/// id) just as easily as `telegram_bot_token` can point at the wrong bot,
/// with the identical silent-forever failure mode -- outbound sends still
/// succeed (to whoever that id actually is), so nothing errors, and the
/// person who's actually messaging the right bot just never gets replies.
/// Prefers `username` (stable, and what a user recognizes at a glance);
/// falls back to `first_name`/`last_name` for chats without one.
pub fn telegram_chat_identity(chat_id: &str) -> Result<String, String> {
    let parsed = telegram_get("getChat", &format!("?chat_id={chat_id}"))?;
    let result = parsed
        .get("result")
        .ok_or_else(|| "telegram getChat: missing result".to_string())?;
    if let Some(username) = result.get("username").and_then(Value::as_str) {
        return Ok(format!("@{username}"));
    }
    let first = result.get("first_name").and_then(Value::as_str);
    let last = result.get("last_name").and_then(Value::as_str);
    match (first, last) {
        (Some(f), Some(l)) => Ok(format!("{f} {l}")),
        (Some(f), None) => Ok(f.to_string()),
        _ => Err("telegram getChat: no username or name on this chat".to_string()),
    }
}

/// Build the `sendMessage` request for a Telegram message carrying inline
/// keyboard buttons (a "card") -- the richer sibling of [`build_request`]'s
/// plain-text Telegram case. `parse_mode` is `HTML` rather than Markdown so
/// arbitrary text (a PR title, an item description) only needs `&`/`<`/`>`
/// escaped rather than the much larger MarkdownV2 escape set. `buttons` is
/// `(label, callback_data)` pairs rendered as one inline keyboard row.
#[must_use]
pub fn build_telegram_card_request(
    target: &str,
    html_text: &str,
    buttons: &[(&str, &str)],
    token: &str,
) -> OutboundRequest {
    OutboundRequest {
        url: flare_channels::send_message_url(flare_channels::TELEGRAM_API_BASE, token),
        auth: None,
        body: flare_channels::card_body(target, html_text, buttons),
    }
}

/// Send a Telegram message with inline keyboard buttons -- the card variant
/// of [`send_message`]. Telegram-only: Slack/Discord buttons are entirely
/// different payload shapes (Block Kit / message components), and nothing
/// else needs them yet.
pub fn send_telegram_card(
    target: &str,
    html_text: &str,
    buttons: &[(&str, &str)],
) -> Result<(), String> {
    let token = telegram_token()?;
    let req = build_telegram_card_request(target, html_text, buttons, &token);
    send(Platform::Telegram, &req)
}

/// Poll Telegram for updates since `offset` (pass `last_update_id + 1`, same
/// convention as the Bot API's own `getUpdates`). Short-polls (`timeout: 0`)
/// since this is called from a fixed-interval supervisor tick rather than a
/// dedicated long-poll thread -- there's nothing to gain from Telegram
/// holding the connection open, only a busy tick thread. `allowed_updates`
/// filters server-side; `supervisor::poll_telegram_approvals` passes both
/// `callback_query` (the PR-approval-card flow) and `message` (the chat
/// channel) in one call, since Telegram allows only one `getUpdates` poller
/// per bot token and advancing the offset confirms every update below it
/// regardless of which filter fetched them.
pub fn get_telegram_updates_filtered(
    offset: i64,
    allowed_updates: &[&str],
) -> Result<Vec<Value>, String> {
    let token = telegram_token()?;
    let url = format!("https://api.telegram.org/bot{}/getUpdates", *token);
    let body = json!({ "offset": offset, "timeout": 0, "allowed_updates": allowed_updates });
    let resp = match http_agent().post(&url).send_json(&body) {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, resp)) => {
            return Err(format!(
                "telegram getUpdates HTTP {code}: {}",
                resp.into_string().unwrap_or_default()
            ));
        }
        Err(e) => return Err(describe_send_error(Platform::Telegram, &e)),
    };
    let parsed: Value = resp
        .into_json()
        .map_err(|e| format!("telegram getUpdates response was not JSON: {e}"))?;
    parsed
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "telegram getUpdates: missing result array".to_string())
}

/// Acknowledge a callback query with a toast shown on the tapped button --
/// required within a short window or Telegram shows the button as
/// stuck/loading client-side.
pub fn answer_telegram_callback(callback_query_id: &str, text: &str) -> Result<(), String> {
    let token = telegram_token()?;
    let req = OutboundRequest {
        url: flare_channels::method_url(
            flare_channels::TELEGRAM_API_BASE,
            &token,
            "answerCallbackQuery",
        ),
        auth: None,
        body: flare_channels::answer_callback_body(callback_query_id, text),
    };
    send(Platform::Telegram, &req)
}

/// Strip the inline keyboard off an already-sent card once its button has
/// been acted on, so a stale message can't be tapped a second time.
pub fn clear_telegram_reply_markup(chat_id: &str, message_id: i64) -> Result<(), String> {
    let token = telegram_token()?;
    let req = OutboundRequest {
        url: flare_channels::method_url(
            flare_channels::TELEGRAM_API_BASE,
            &token,
            "editMessageReplyMarkup",
        ),
        auth: None,
        body: flare_channels::clear_markup_body(chat_id, message_id),
    };
    send(Platform::Telegram, &req)
}

/// Bot commands advertised via Telegram `setMyCommands`, sourced from the
/// same [`crate::mcp_server::chat::CHAT_COMMAND_SPECS`] the `/help` text and
/// the command dispatch read — one list, three consumers.
pub fn telegram_bot_commands() -> Vec<flare_channels::BotCommand> {
    crate::mcp_server::chat::CHAT_COMMAND_SPECS
        .iter()
        .filter_map(|(name, _, desc)| flare_channels::BotCommand::new(name, desc))
        .collect()
}

/// Clear a stale webhook mapping left by a previous process so `getUpdates`
/// polling does not hit HTTP 409. Keeps the server-side backlog
/// (`drop_pending_updates = false`): the supervisor's persisted offset
/// replays it instead of silently losing messages.
pub fn clear_telegram_webhook() -> Result<(), String> {
    let token = telegram_token()?;
    let req = OutboundRequest {
        url: flare_channels::method_url(flare_channels::TELEGRAM_API_BASE, &token, "deleteWebhook"),
        auth: None,
        body: flare_channels::delete_webhook_body(false),
    };
    send(Platform::Telegram, &req)
}

/// Advertise the command menu. No-op when the specs table is empty.
pub fn register_telegram_commands() -> Result<(), String> {
    let commands = telegram_bot_commands();
    if commands.is_empty() {
        return Ok(());
    }
    let token = telegram_token()?;
    let req = OutboundRequest {
        url: flare_channels::method_url(flare_channels::TELEGRAM_API_BASE, &token, "setMyCommands"),
        auth: None,
        body: flare_channels::set_commands_body(&commands),
    };
    send(Platform::Telegram, &req)
}

/// One-time Telegram startup: validate the token (`getMe`, fail fast),
/// clear a stale webhook mapping, and advertise the command menu. Called on
/// the first supervisor tick; every step is non-fatal (a failure logs and
/// retries on the next tick, e.g. for a token added later), and success is
/// remembered for the life of the process.
pub(crate) fn ensure_telegram_ready() {
    static DONE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if DONE.get().is_some() {
        return;
    }
    if let Err(e) = telegram_bot_identity() {
        eprintln!("agentflare-supervisor: telegram startup: token check failed: {e}");
        return;
    }
    if let Err(e) = clear_telegram_webhook() {
        eprintln!("agentflare-supervisor: telegram startup: {e}");
        return;
    }
    if let Err(e) = register_telegram_commands() {
        eprintln!("agentflare-supervisor: telegram startup: {e}");
        return;
    }
    let _ = DONE.set(());
}

/// Process-wide realtime bus for [`flare_channels::ChannelEvent`]s. The
/// Telegram poller publishes `Inbound`/`Settled`, chat turns publish
/// `Typing`/`Outbound`, and consumers (dashboard SSE, future gateway WS)
/// subscribe — no polling, no per-client work.
static CHAT_BUS: std::sync::OnceLock<flare_channels::ChatBus> = std::sync::OnceLock::new();

/// Clone of the shared realtime bus. Publishing never fails (no listeners
/// is fine); subscribers lag-drop like the dashboard `/events` stream.
pub fn chat_bus() -> flare_channels::ChatBus {
    CHAT_BUS
        .get_or_init(flare_channels::ChatBus::default)
        .clone()
}

/// [`flare_channels::TokenProvider`] backed by the encrypted vault: the
/// crate asks for a channel alias, this maps it to the matching
/// [`Platform::secret_name`]. `telegram` and `telegram.<alias>` both resolve
/// to `telegram_bot_token` (single-bot today, alias-ready for multi-bot);
/// unknown platforms resolve to `None` rather than erroring.
pub struct VaultTokens;

impl flare_channels::TokenProvider for VaultTokens {
    fn token_for(&self, channel: &str) -> Option<String> {
        let base = channel.split('.').next().unwrap_or(channel);
        let name = Platform::parse(base)?.secret_name();
        crate::vault::get_secret(name)
            .ok()
            .flatten()
            .map(|s| s.to_string())
    }
}

/// Named live channel handles over `flare-channels` transports. Immutable
/// after first build, so no lock is held across `.await` sends. Single
/// `telegram` entry for now — the supervisor's poller and the dashboard
/// send endpoint share it instead of each hand-rolling HTTP.
static CHANNEL_HANDLES: std::sync::OnceLock<
    std::collections::HashMap<String, std::sync::Arc<dyn flare_channels::Channel>>,
> = std::sync::OnceLock::new();

/// Look up a live channel by alias (`telegram`). `None` for unconfigured
/// platforms — the caller decides the status code, not this.
pub fn channel_handle(alias: &str) -> Option<std::sync::Arc<dyn flare_channels::Channel>> {
    CHANNEL_HANDLES
        .get_or_init(|| {
            let mut map = std::collections::HashMap::new();
            let config = flare_channels::TelegramConfig::default();
            map.insert(
                config.alias.clone(),
                std::sync::Arc::new(flare_channels::TelegramChannel::new(
                    config,
                    std::sync::Arc::new(VaultTokens),
                )) as std::sync::Arc<dyn flare_channels::Channel>,
            );
            map
        })
        .get(alias)
        .cloned()
}

/// Send one message through the named live channel and mirror it on the
/// realtime bus. New callers (dashboard send endpoint) use this; the
/// supervisor's existing `send_message` path is untouched.
pub async fn send_chat(alias: &str, target: &str, text: &str) -> Result<(), String> {
    let Some(handle) = channel_handle(alias) else {
        return Err(format!("unknown chat channel: {alias}"));
    };
    handle
        .send(flare_channels::SendMessage::new(target, text))
        .await
        .map_err(|e| e.to_string())?;
    chat_bus().publish(flare_channels::ChannelEvent::Outbound(
        flare_channels::SendMessage::new(target, text),
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "sends a real Telegram message to whatever chat is configured in the local vault; run manually with: cargo test --bin agentflare channels::tests::manual_send_test_card -- --ignored --nocapture"]
    fn manual_send_test_card() {
        let chat_id = crate::vault::get_secret("telegram_notify_chat_id")
            .expect("vault read failed")
            .expect("telegram_notify_chat_id not configured in vault");
        let result = send_telegram_card(
            &chat_id,
            "\u{1F514} <b>agentflare</b> test card\nThis is a manual test of the new rich-card notification (repo/item/PR + Approve button).",
            &[("\u{2705} Approve", "approve:test/repo#1")],
        );
        println!("send_telegram_card result: {result:?}");
        assert!(result.is_ok(), "send failed: {result:?}");
    }

    #[test]
    fn parse_platform_is_case_insensitive_and_rejects_unknown() {
        assert_eq!(Platform::parse("telegram"), Some(Platform::Telegram));
        assert_eq!(Platform::parse("Slack"), Some(Platform::Slack));
        assert_eq!(Platform::parse("DISCORD"), Some(Platform::Discord));
        assert_eq!(Platform::parse("myspace"), None);
    }

    #[test]
    fn secret_name_per_platform() {
        assert_eq!(Platform::Telegram.secret_name(), "telegram_bot_token");
        assert_eq!(Platform::Slack.secret_name(), "slack_bot_token");
        assert_eq!(Platform::Discord.secret_name(), "discord_bot_token");
    }

    #[test]
    fn telegram_request_puts_token_in_url_and_no_auth_header() {
        let r = build_request(Platform::Telegram, "12345", "hi", "TOK");
        assert_eq!(r.url, "https://api.telegram.org/botTOK/sendMessage");
        assert!(r.auth.is_none());
        assert_eq!(r.body["chat_id"], "12345");
        assert_eq!(r.body["text"], "hi");
    }

    #[test]
    fn slack_request_uses_bearer_auth() {
        let r = build_request(Platform::Slack, "C123", "hi", "xoxb-TOK");
        assert_eq!(r.url, "https://slack.com/api/chat.postMessage");
        assert_eq!(r.auth.as_deref(), Some("Bearer xoxb-TOK"));
        assert_eq!(r.body["channel"], "C123");
        // Plain-text fallback stays present (Slack notification/accessibility
        // copy for clients that don't render blocks)...
        assert_eq!(r.body["text"], "hi");
        // ...alongside a Block Kit section carrying the same text as mrkdwn,
        // so it renders as a card rather than a bare line.
        assert_eq!(r.body["blocks"][0]["type"], "section");
        assert_eq!(r.body["blocks"][0]["text"]["type"], "mrkdwn");
        assert_eq!(r.body["blocks"][0]["text"]["text"], "hi");
    }

    #[test]
    fn discord_request_uses_bot_auth_and_channel_in_url() {
        let r = build_request(Platform::Discord, "999", "hi", "TOK");
        assert_eq!(r.url, "https://discord.com/api/v10/channels/999/messages");
        assert_eq!(r.auth.as_deref(), Some("Bot TOK"));
        assert_eq!(r.body["embeds"][0]["description"], "hi");
    }

    #[test]
    fn interpret_telegram_and_discord_are_status_only() {
        assert!(interpret_response(Platform::Telegram, 200, "").is_ok());
        assert!(interpret_response(Platform::Discord, 200, "{}").is_ok());
        assert!(interpret_response(Platform::Discord, 500, "boom").is_err());
        assert!(interpret_response(Platform::Telegram, 403, "forbidden").is_err());
    }

    #[test]
    fn interpret_slack_checks_the_ok_field_even_on_http_200() {
        assert!(interpret_response(Platform::Slack, 200, r#"{"ok":true,"ts":"1"}"#).is_ok());
        let err = interpret_response(
            Platform::Slack,
            200,
            r#"{"ok":false,"error":"channel_not_found"}"#,
        )
        .unwrap_err();
        assert!(
            err.contains("channel_not_found"),
            "error should surface Slack's reason: {err}"
        );
    }

    #[test]
    fn describe_send_error_never_leaks_the_url_or_token() {
        // Force a real transport-level failure (connection refused — nothing
        // listens on 127.0.0.1:1) against a URL that embeds a fake bot token,
        // the same shape Telegram's real URL takes. This is not a flaky
        // network test: the connection is refused locally and immediately,
        // with a short timeout as a backstop.
        let token = "SUPER-SECRET-TELEGRAM-TOKEN";
        let url = format!("http://127.0.0.1:1/bot{token}/sendMessage");
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_millis(500))
            .build();
        let err = agent
            .post(&url)
            .send_json(json!({}))
            .expect_err("connecting to a closed local port must fail");
        // Sanity: confirm this really is the transport-error arm (not an HTTP
        // status), i.e. the same arm `send()` routes through `describe_send_error`.
        assert!(
            matches!(err, ureq::Error::Transport(_)),
            "expected a transport-level error, got: {err}"
        );

        let msg = describe_send_error(Platform::Telegram, &err);
        assert!(
            !msg.contains(token),
            "error message must not leak the bot token: {msg}"
        );
        assert!(
            !msg.contains(&url),
            "error message must not leak the request URL: {msg}"
        );
        assert!(
            msg.contains("Telegram"),
            "error message should name the platform: {msg}"
        );
    }

    #[test]
    fn send_message_without_a_configured_token_errors_clearly() {
        // Isolated home dir so this can't read the developer's real vault --
        // without it, a configured+unlocked telegram_bot_token would make
        // this test send a real Telegram message using real credentials.
        crate::paths::test_support::with_temp_home(|| {
            let err = send_message(Platform::Telegram, "123", "hi").unwrap_err();
            assert!(
                err.contains("telegram_bot_token"),
                "should name the missing secret: {err}"
            );
        });
    }

    #[test]
    fn vault_tokens_resolves_aliases_and_rejects_unknown() {
        use flare_channels::TokenProvider as _;
        crate::paths::test_support::with_temp_home(|| {
            let tokens = VaultTokens;
            // Empty vault: known platforms resolve to None (no panic, no
            // error) so transports fail later with a clear message.
            assert!(tokens.token_for("telegram").is_none());
            assert!(tokens.token_for("telegram.home").is_none());
            assert!(tokens.token_for("slack").is_none());
            assert!(tokens.token_for("discord").is_none());
            // Not a platform prefix: must not match.
            assert!(tokens.token_for("telegram-bot").is_none());
            assert!(tokens.token_for("nope").is_none());
            assert!(tokens.token_for("").is_none());
        });
    }

    #[test]
    fn channel_registry_exposes_telegram_only() {
        assert!(channel_handle("telegram").is_some());
        assert!(channel_handle("slack").is_none());
        assert!(channel_handle("telegram.home").is_none());
    }

    #[tokio::test]
    async fn send_chat_rejects_unknown_channel_without_touching_network() {
        let err = send_chat("nope", "123", "hi")
            .await
            .expect_err("unknown channel must error");
        assert!(err.contains("nope"), "should name the channel: {err}");
    }

    #[test]
    fn telegram_bot_commands_mirror_chat_specs() {
        let commands = telegram_bot_commands();
        let names: Vec<&str> = commands.iter().map(|c| c.command.as_str()).collect();
        assert_eq!(names, vec!["status", "project", "new", "help"]);
    }

    #[test]
    fn telegram_startup_helpers_fail_cleanly_without_a_token() {
        // No network touched: with no vault present every step fails at the
        // token lookup, and ensure_telegram_ready retries on later ticks
        // (rather than latching) so a token added later still takes effect.
        crate::paths::test_support::with_temp_home(|| {
            assert!(clear_telegram_webhook().is_err());
            assert!(register_telegram_commands().is_err());
            ensure_telegram_ready();
        });
    }
}
