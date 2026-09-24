//! Inter-agent messaging endpoints (see `crate::messages`).
//!
//! - `GET /api/sessions` -- live agent sessions.
//! - `GET /api/messages?to=KEY[&all=true][&limit=N]` -- a mailbox, read-only.
//! - `POST /api/messages` `{to, body, reply_to?}` -- send as the local human
//!   (`human:<user>`); a dashboard caller can't speak as an agent session.
//! - `GET /api/messages/stream?to=KEY[&take=true][&after=ID]` -- SSE, one
//!   `message` event per message. Sends made in this process are pushed off
//!   the in-process bus at once; sends from other processes (agents' MCP
//!   servers, the CLI) are picked up by a 500ms db poll. With `take=true`
//!   each streamed message is marked delivered (the stream *is* the
//!   recipient's delivery path, e.g. `agentflare message watch`); otherwise
//!   the stream only observes, starting after `after` (default: now).

use crate::messages;
use axum::{
    Json, Router,
    extract::Query,
    http::StatusCode,
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use serde::Deserialize;

pub fn router() -> Router {
    Router::new()
        .route("/api/sessions", get(sessions_handler))
        .route("/api/messages", get(inbox_handler).post(send_handler))
        .route("/api/messages/stream", get(stream_handler))
}

fn internal(e: impl std::fmt::Display) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

/// Runs a db read off the async runtime; any failure becomes a 500.
async fn blocking<T: serde::Serialize + Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Response {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e),
    }
}

async fn sessions_handler() -> Response {
    blocking(|| {
        let conn = crate::db::open().map_err(|e| e.to_string())?;
        crate::sessions::list_live(&conn, crate::claims::now()).map_err(|e| e.to_string())
    })
    .await
}

#[derive(Deserialize)]
struct InboxQuery {
    to: String,
    #[serde(default)]
    all: bool,
    limit: Option<usize>,
}

async fn inbox_handler(Query(q): Query<InboxQuery>) -> Response {
    blocking(move || {
        let conn = crate::db::open().map_err(|e| e.to_string())?;
        messages::inbox(&conn, &q.to, !q.all, q.limit.unwrap_or(50).clamp(1, 500))
            .map_err(|e| e.to_string())
    })
    .await
}

#[derive(Deserialize)]
struct SendRequest {
    to: String,
    body: String,
    reply_to: Option<i64>,
}

async fn send_handler(Json(req): Json<SendRequest>) -> Response {
    if req.to.trim().is_empty() || req.body.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "to and body must not be empty").into_response();
    }
    let sent = tokio::task::spawn_blocking(move || {
        let conn = crate::db::open().map_err(|e| e.to_string())?;
        crate::mcp_server::AgentflareMcp::default()
            .message_as(
                &conn,
                &messages::identity::human_key(),
                crate::mcp_server::types::MessageRequest {
                    action: "send".into(),
                    to: Some(req.to),
                    body: Some(req.body),
                    reply_to: req.reply_to,
                    ..Default::default()
                },
            )
            .map_err(|e| e.message.to_string())
    })
    .await;
    match sent {
        Ok(Ok(json)) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json,
        )
            .into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct StreamQuery {
    to: Option<String>,
    #[serde(default)]
    take: bool,
    after: Option<i64>,
}

const STREAM_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// One poll of the stream's source: the recipient's undelivered mail when
/// taking, else everything past `after` (filtered to `to` when given).
fn next_batch(
    conn: &mut Option<rusqlite::Connection>,
    to: Option<&str>,
    take: bool,
    after: i64,
) -> Vec<messages::Message> {
    if conn.is_none() {
        *conn = crate::db::open().ok();
    }
    let Some(c) = conn.as_ref() else {
        return vec![];
    };
    match (take, to) {
        (true, Some(to)) => {
            messages::take_undelivered(c, to, messages::MAX_BATCH, crate::claims::now())
        }
        _ => messages::since(c, to, after, 100),
    }
    .unwrap_or_default()
}

async fn stream_handler(
    Query(q): Query<StreamQuery>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(64);
    let to = q.to.filter(|t| !t.trim().is_empty());
    let take = q.take && to.is_some();
    tokio::spawn(async move {
        let mut bus = messages::bus().subscribe();
        let mut conn: Option<rusqlite::Connection> = None;
        let mut after = match q.after {
            Some(a) => a,
            None if take => 0,
            None => {
                let (c, max) = tokio::task::spawn_blocking(move || {
                    let c = crate::db::open().ok();
                    let max = c
                        .as_ref()
                        .and_then(|c| messages::max_id(c).ok())
                        .unwrap_or(0);
                    (c, max)
                })
                .await
                .unwrap_or((None, 0));
                conn = c;
                max
            }
        };
        loop {
            let to_q = to.clone();
            let (c, batch) = match tokio::task::spawn_blocking(move || {
                let batch = next_batch(&mut conn, to_q.as_deref(), take, after);
                (conn, batch)
            })
            .await
            {
                Ok(v) => v,
                Err(_) => return,
            };
            conn = c;
            for m in batch {
                after = after.max(m.id);
                let Ok(data) = serde_json::to_string(&m) else {
                    continue;
                };
                let event = Event::default()
                    .event("message")
                    .id(m.id.to_string())
                    .data(data);
                if tx.send(event).await.is_err() {
                    return;
                }
            }
            tokio::select! {
                _ = bus.recv() => {}
                _ = tokio::time::sleep(STREAM_POLL) => {}
                _ = tx.closed() => return,
            }
        }
    });
    let stream = tokio_stream::StreamExt::map(tokio_stream::wrappers::ReceiverStream::new(rx), Ok);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_queue() -> agentflare_jobs::Queue {
        let dir = tempfile::tempdir().unwrap().keep();
        agentflare_jobs::Queue::open_memory(dir.join("logs")).unwrap()
    }

    #[test]
    fn send_then_stream_and_list_sessions() {
        crate::paths::test_support::with_temp_home(|| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                use tokio_stream::StreamExt as _;
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .unwrap();
                let addr = listener.local_addr().unwrap();
                tokio::spawn(async move {
                    axum::serve(listener, super::super::server::router(test_queue()))
                        .await
                        .unwrap();
                });
                let client = reqwest::Client::new();

                let sessions = client
                    .get(format!("http://{addr}/api/sessions"))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(sessions.status(), StatusCode::OK);
                assert!(
                    sessions
                        .json::<serde_json::Value>()
                        .await
                        .unwrap()
                        .is_array()
                );

                let bad = client
                    .post(format!("http://{addr}/api/messages"))
                    .json(&serde_json::json!({"to": "", "body": "x"}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

                // Taking stream on a mailbox, opened before the send.
                let resp = client
                    .get(format!(
                        "http://{addr}/api/messages/stream?to=human:probe-7c1&take=true"
                    ))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
                let sent = client
                    .post(format!("http://{addr}/api/messages"))
                    .json(&serde_json::json!({"to": "human:probe-7c1", "body": "stream probe"}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(sent.status(), StatusCode::OK);

                let mut stream = resp.bytes_stream();
                let mut seen = String::new();
                for _ in 0..20 {
                    match tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
                        .await
                    {
                        Ok(Some(Ok(chunk))) => {
                            seen.push_str(&String::from_utf8_lossy(&chunk));
                            if seen.contains("stream probe") {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
                assert!(seen.contains("stream probe"), "got: {seen}");
                assert!(seen.contains("event: message"));
                // Taken by the stream: delivered.
                let conn = crate::db::open().unwrap();
                assert!(!messages::has_undelivered(&conn, "human:probe-7c1").unwrap());
            });
        });
    }
}
