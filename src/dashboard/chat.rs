//! Realtime chat endpoints over the shared `flare-channels` bus.
//!
//! Split out of `server.rs` to keep that file under the LOC gate
//! (`scripts/loc-gate.sh`): `GET /api/chat/events` streams channel events,
//! `POST /api/chat/send` sends one bot message. No per-client state — every
//! subscriber reads the same process-wide bus.

use axum::{
    Json, Router,
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, post},
};
use serde::Deserialize;

pub fn router() -> Router {
    Router::new()
        .route("/api/chat/events", get(chat_events_handler))
        .route("/api/chat/send", post(send_chat_handler))
}

/// `GET /api/chat/events` — realtime channel-event stream (`Inbound`,
/// `Typing`, `Outbound`, `Settled` as JSON `data:` frames). Subscribes to
/// the shared [`crate::channels::chat_bus`], so N connected tabs cost one
/// publish path rather than N polls. A newly connected client sees nothing
/// until the next Telegram turn; the view shows "connecting…" until then.
async fn chat_events_handler()
-> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = crate::channels::chat_bus().subscribe();
    // Drop lagged/errored frames — the next event supersedes them, same
    // policy as the `/events` snapshot stream in `server.rs`.
    let stream = tokio_stream::StreamExt::filter_map(
        tokio_stream::wrappers::BroadcastStream::new(rx),
        |msg| match msg {
            Ok(event) => serde_json::to_string(&event)
                .ok()
                .map(|data| Ok(Event::default().data(data))),
            Err(_) => None,
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// Body for `POST /api/chat/send`: one outbound Telegram message as the bot.
#[derive(Deserialize)]
struct SendChatRequest {
    target: String,
    text: String,
}

/// `POST /api/chat/send` — send one Telegram message as the bot. Gated to
/// the configured notify chat (the same authorization the supervisor
/// applies to inbound messages: this bot talks to its operator, nobody
/// else), so a stray dashboard caller cannot message arbitrary chat ids.
/// Goes through the shared `flare-channels` transport, not a hand-rolled
/// POST, and mirrors the send on the realtime bus.
async fn send_chat_handler(Json(req): Json<SendChatRequest>) -> Response {
    let target = req.target.trim();
    let text = req.text.trim();
    if target.is_empty() || text.is_empty() {
        return (StatusCode::BAD_REQUEST, "target and text must not be empty").into_response();
    }
    let expected = crate::vault::get_secret(crate::supervisor::TELEGRAM_NOTIFY_CHAT_ID_SECRET)
        .ok()
        .flatten()
        .map(|s| s.to_string());
    if expected.as_deref() != Some(target) {
        return (
            StatusCode::FORBIDDEN,
            "chat send is limited to the configured notify chat",
        )
            .into_response();
    }
    match crate::channels::send_chat("telegram", target, text).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("telegram send failed: {e}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentflare_jobs::Queue;

    fn test_queue() -> Queue {
        // Same `.keep()` reasoning as `server.rs`'s helper: the returned
        // `Queue`'s `log_dir` must outlive this function.
        let dir = tempfile::tempdir().unwrap().keep();
        Queue::open_memory(dir.join("logs")).unwrap()
    }

    /// Serve the full dashboard router (not just this submodule) so the
    /// merge wiring in `server.rs` is exercised too.
    fn full_router() -> axum::Router {
        super::super::server::router(test_queue())
    }

    #[tokio::test]
    async fn chat_events_endpoint_streams_bus_events() {
        use tokio_stream::StreamExt as _;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, full_router()).await.unwrap();
        });
        let resp = reqwest::get(format!("http://{addr}/api/chat/events"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(
            content_type.starts_with("text/event-stream"),
            "expected SSE content type, got: {content_type}"
        );
        // Unique marker: the process-wide bus is shared with every other
        // test in this binary, so only this target proves the frame came
        // from our publish. Headers already arrived, so the handler has
        // subscribed and broadcast delivery is deterministic from here.
        let probe = "chat-events-probe-7f3a9c";
        crate::channels::chat_bus().publish(flare_channels::ChannelEvent::Typing {
            channel: "telegram".to_string(),
            target: probe.to_string(),
        });
        let mut stream = resp.bytes_stream();
        let mut seen = false;
        for _ in 0..20 {
            match tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    let text = String::from_utf8_lossy(&chunk).into_owned();
                    if text.contains(probe) {
                        seen = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(seen, "expected our bus event on the SSE stream");
    }

    #[tokio::test]
    async fn chat_send_endpoint_rejects_empty_and_unauthorized() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, full_router()).await.unwrap();
        });
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/api/chat/send");
        // Empty target/text never reaches the vault or the network.
        for body in [
            serde_json::json!({"target": "", "text": "hi"}),
            serde_json::json!({"target": "42", "text": "  "}),
        ] {
            let resp = client.post(&url).json(&body).send().await.unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        }
        // A chat id nobody configured is forbidden before any network
        // happens — the probe value cannot collide with a real setup.
        let resp = client
            .post(&url)
            .json(&serde_json::json!({"target": "000000-chat-send-probe", "text": "hi"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
}
