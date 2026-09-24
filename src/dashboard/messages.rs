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
//!   each streamed message is marked delivered (and handed back if the
//!   client disconnects before it's known received -- the stream *is* the
//!   recipient's delivery path, e.g. `agentflare message watch`). That is
//!   at-least-once: a message in flight at a disconnect may be delivered
//!   again on the next take, never lost (see `pump_taken`); otherwise
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

/// One poll of the observe-only stream's source: everything past `after`
/// (filtered to `to` when given).
fn next_batch(
    conn: &mut Option<rusqlite::Connection>,
    to: Option<&str>,
    after: i64,
) -> Vec<messages::Message> {
    if conn.is_none() {
        *conn = crate::db::open().ok();
    }
    let Some(c) = conn.as_ref() else {
        return vec![];
    };
    messages::since(c, to, after, 100).unwrap_or_default()
}

/// Takes (marks delivered) `to`'s single oldest undelivered message.
fn take_one(conn: &mut Option<rusqlite::Connection>, to: &str) -> Option<messages::Message> {
    if conn.is_none() {
        *conn = crate::db::open().ok();
    }
    messages::take_undelivered(conn.as_ref()?, to, 1, crate::claims::now())
        .ok()?
        .into_iter()
        .next()
}

/// Hands taken-but-possibly-unreceived messages back to the mailbox.
async fn requeue_unsent(conn: Option<rusqlite::Connection>, ids: Vec<i64>) {
    if ids.is_empty() {
        return;
    }
    let _ = tokio::task::spawn_blocking(move || {
        let conn = conn.or_else(|| crate::db::open().ok());
        if let Some(c) = conn {
            let _ = messages::requeue(&c, &ids);
        }
    })
    .await;
}

fn message_event(m: &messages::Message) -> Option<Event> {
    let data = serde_json::to_string(m).ok()?;
    Some(
        Event::default()
            .event("message")
            .id(m.id.to_string())
            .data(data),
    )
}

/// Channel capacity of a `take=true` stream. Kept at 1 so a granted send
/// permit proves the previously sent event was pulled off the channel by
/// the SSE body.
const TAKE_CHANNEL_CAPACITY: usize = 1;

/// Taken ids that may not have reached the client yet: the one sitting in
/// the channel plus the one the SSE body pulled but may not have flushed.
const TAKE_IN_FLIGHT: usize = TAKE_CHANNEL_CAPACITY + 1;

/// The `take=true` delivery loop, at-least-once: a message is taken (marked
/// delivered) only after a send permit is held, one at a time, and the last
/// [`TAKE_IN_FLIGHT`] taken ids are remembered for as long as the stream is
/// open, since SSE gives no receipt. When the client goes away, those are
/// requeued -- so a disconnect can deliver a message twice (it may have
/// reached the client just before the drop), which is preferred over
/// marking it delivered and losing it. `agentflare message watch` skips ids
/// it already printed when it reconnects.
async fn pump_taken(tx: tokio::sync::mpsc::Sender<Event>, to: String) {
    let mut bus = messages::bus().subscribe();
    let mut conn: Option<rusqlite::Connection> = None;
    let mut in_flight: std::collections::VecDeque<i64> = std::collections::VecDeque::new();
    loop {
        // A granted permit means the channel has room again, i.e. the
        // previous event was pulled by the SSE body.
        let Ok(permit) = tx.reserve().await else {
            requeue_unsent(conn, in_flight.into()).await;
            return;
        };
        let to_q = to.clone();
        let Ok((c, taken)) = tokio::task::spawn_blocking(move || {
            let m = take_one(&mut conn, &to_q);
            (conn, m)
        })
        .await
        else {
            return;
        };
        conn = c;
        if let Some(m) = taken {
            // Unserializable: nothing could ever be sent for it; leave it
            // delivered rather than spin on it.
            if let Some(event) = message_event(&m) {
                permit.send(event);
                in_flight.push_back(m.id);
                while in_flight.len() > TAKE_IN_FLIGHT {
                    in_flight.pop_front();
                }
            }
            continue;
        }
        drop(permit);
        tokio::select! {
            _ = bus.recv() => {}
            // Being pulled off the channel doesn't prove the client got
            // it, so in-flight ids stay requeueable until the stream closes.
            () = tokio::time::sleep(STREAM_POLL) => {}
            () = tx.closed() => {
                requeue_unsent(conn, in_flight.into()).await;
                return;
            }
        }
    }
}

/// The observe-only loop: streams everything past `after`, marks nothing.
async fn pump_observed(
    tx: tokio::sync::mpsc::Sender<Event>,
    to: Option<String>,
    after: Option<i64>,
) {
    let mut bus = messages::bus().subscribe();
    let mut conn: Option<rusqlite::Connection> = None;
    let mut after = match after {
        Some(a) => a,
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
            let batch = next_batch(&mut conn, to_q.as_deref(), after);
            (conn, batch)
        })
        .await
        {
            Ok(v) => v,
            Err(_) => return,
        };
        conn = c;
        for m in &batch {
            after = after.max(m.id);
            let Some(event) = message_event(m) else {
                continue;
            };
            if tx.send(event).await.is_err() {
                return;
            }
        }
        tokio::select! {
            _ = bus.recv() => {}
            _ = tokio::time::sleep(STREAM_POLL) => {}
            () = tx.closed() => return,
        }
    }
}

async fn stream_handler(
    Query(q): Query<StreamQuery>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let to = q.to.filter(|t| !t.trim().is_empty());
    let rx = match to {
        Some(to) if q.take => {
            let (tx, rx) = tokio::sync::mpsc::channel::<Event>(TAKE_CHANNEL_CAPACITY);
            tokio::spawn(pump_taken(tx, to));
            rx
        }
        to => {
            let (tx, rx) = tokio::sync::mpsc::channel::<Event>(64);
            tokio::spawn(pump_observed(tx, to, q.after));
            rx
        }
    };
    let stream = tokio_stream::StreamExt::map(tokio_stream::wrappers::ReceiverStream::new(rx), Ok);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_take_stream_requeues_what_it_may_not_have_delivered() {
        crate::paths::test_support::with_temp_home(|| {
            const TO: &str = "human:probe-requeue";
            let conn = crate::db::open().unwrap();
            for body in ["one", "two", "three"] {
                messages::send(&conn, "x:1", TO, body, None, 100, |_| Err(String::new())).unwrap();
            }
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(TAKE_CHANNEL_CAPACITY);
                let pump = tokio::spawn(pump_taken(tx, TO.to_string()));
                // The client pulls one event, then goes away.
                assert!(rx.recv().await.is_some());
                drop(rx);
                tokio::time::timeout(std::time::Duration::from_secs(5), pump)
                    .await
                    .expect("the pump stops once the client is gone")
                    .unwrap();
            });
            // Nothing is lost: everything but at most the one event the
            // client pulled is back in the mailbox (a duplicate of that one
            // is allowed).
            let back = messages::take_undelivered(&conn, TO, 10, 200).unwrap();
            let bodies: Vec<&str> = back.iter().map(|m| m.body.as_str()).collect();
            assert!(bodies.contains(&"two"), "{bodies:?}");
            assert!(bodies.contains(&"three"), "{bodies:?}");
        });
    }

    #[test]
    fn a_pulled_event_stays_requeueable_through_an_idle_poll() {
        crate::paths::test_support::with_temp_home(|| {
            const TO: &str = "human:probe-idle";
            let conn = crate::db::open().unwrap();
            messages::send(&conn, "x:1", TO, "only", None, 100, |_| Err(String::new())).unwrap();
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(TAKE_CHANNEL_CAPACITY);
                let pump = tokio::spawn(pump_taken(tx, TO.to_string()));
                // Pulled off the channel, but not proven to reach the client:
                // the stream idles past a poll, then the client drops.
                assert!(rx.recv().await.is_some());
                tokio::time::sleep(STREAM_POLL * 3).await;
                drop(rx);
                tokio::time::timeout(std::time::Duration::from_secs(5), pump)
                    .await
                    .expect("the pump stops once the client is gone")
                    .unwrap();
            });
            let back = messages::take_undelivered(&conn, TO, 10, 200).unwrap();
            assert_eq!(back.len(), 1, "{back:?}");
        });
    }

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
