//! Outbound channels: send a plain-text message out to a chat platform
//! (Telegram / Slack / Discord). This is the "outward" half of the channels
//! effort — a one-shot blocking POST that fits agentflare's sync/`ureq` model,
//! callable by an agent mid-run (MCP tool) or from the CLI. Bot tokens live in
//! the encrypted `gateway_secrets` store; the inbound daemon (flared) reuses
//! this same path to send its replies.
//!
//! Request shapes per platform:
//! - Telegram: `POST {base}/bot{token}/sendMessage`  body `{chat_id, text}`     (token in URL)
//! - Slack:    `POST slack.com/api/chat.postMessage`  body `{channel, text}`    (Authorization: Bearer)
//! - Discord:  `POST discord.com/api/v10/channels/{id}/messages`  body `{content}` (Authorization: Bot)

use serde_json::{json, Value};

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

/// Build the platform-specific send request for a plain-text message.
#[must_use]
pub fn build_request(platform: Platform, target: &str, text: &str, token: &str) -> OutboundRequest {
    match platform {
        // Token goes in the URL path; no auth header.
        Platform::Telegram => OutboundRequest {
            url: format!("https://api.telegram.org/bot{token}/sendMessage"),
            auth: None,
            body: json!({ "chat_id": target, "text": text }),
        },
        Platform::Slack => OutboundRequest {
            url: "https://slack.com/api/chat.postMessage".to_string(),
            auth: Some(format!("Bearer {token}")),
            body: json!({ "channel": target, "text": text }),
        },
        // Discord uses the literal `Bot ` auth prefix (not `Bearer`).
        Platform::Discord => OutboundRequest {
            url: format!("https://discord.com/api/v10/channels/{target}/messages"),
            auth: Some(format!("Bot {token}")),
            body: json!({ "content": text }),
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

/// Execute a built request over blocking HTTP and interpret the outcome.
fn send(platform: Platform, req: &OutboundRequest) -> Result<(), String> {
    let mut r = ureq::post(&req.url);
    if let Some(auth) = &req.auth {
        r = r.set("Authorization", auth);
    }
    // ureq returns non-2xx as `Err(Status(..))`; capture status+body from both
    // arms so `interpret_response` (e.g. Slack's `ok` field) sees the payload.
    let (status, body) = match r.send_json(&req.body) {
        Ok(resp) => (resp.status(), resp.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, resp)) => (code, resp.into_string().unwrap_or_default()),
        Err(e) => return Err(format!("request to {} failed: {e}", req.url)),
    };
    interpret_response(platform, status, &body)
}

/// Resolve the platform's bot token from the encrypted `gateway_secrets` store
/// and send `text` to `target`. The one entry point CLI and MCP both call.
pub fn send_message(
    conn: &rusqlite::Connection,
    platform: Platform,
    target: &str,
    text: &str,
) -> Result<(), String> {
    let name = platform.secret_name();
    let token = crate::gateway_secrets::get_secret(conn, name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!("no {name} configured — store the bot token as the gateway secret '{name}' first")
        })?;
    let req = build_request(platform, target, text, &token);
    send(platform, &req)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(r.body["text"], "hi");
    }

    #[test]
    fn discord_request_uses_bot_auth_and_channel_in_url() {
        let r = build_request(Platform::Discord, "999", "hi", "TOK");
        assert_eq!(r.url, "https://discord.com/api/v10/channels/999/messages");
        assert_eq!(r.auth.as_deref(), Some("Bot TOK"));
        assert_eq!(r.body["content"], "hi");
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
        let err = interpret_response(Platform::Slack, 200, r#"{"ok":false,"error":"channel_not_found"}"#)
            .unwrap_err();
        assert!(err.contains("channel_not_found"), "error should surface Slack's reason: {err}");
    }

    #[test]
    fn send_message_without_a_configured_token_errors_clearly() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::gateway_secrets::migrate(&conn).unwrap();
        let err = send_message(&conn, Platform::Telegram, "123", "hi").unwrap_err();
        assert!(
            err.contains("telegram_bot_token"),
            "should name the missing secret: {err}"
        );
    }
}
