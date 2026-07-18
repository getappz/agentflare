use axum::{
    Router,
    extract::Query,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use axum::response::sse::{Event, KeepAlive, Sse};

#[derive(RustEmbed)]
#[folder = "dashboard/web/"]
struct WebAssets;

async fn claims_handler() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], crate::dashboard::data::claims_json())
        .into_response()
}

#[derive(Deserialize)]
struct WorkspaceScope {
    workspace_id: String,
}

async fn pm_workspaces_handler() -> Response {
    ([(header::CONTENT_TYPE, "application/json")], crate::dashboard::data::workspaces_json())
        .into_response()
}

async fn pm_projects_handler(Query(q): Query<WorkspaceScope>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::dashboard::data::projects_json(&q.workspace_id),
    )
        .into_response()
}

#[derive(Deserialize)]
struct ProjectScope {
    project_id: String,
}

async fn pm_items_handler(Query(q): Query<ProjectScope>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::dashboard::data::items_json(&q.project_id),
    )
        .into_response()
}

async fn pm_states_handler(Query(q): Query<ProjectScope>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::dashboard::data::states_json(&q.project_id),
    )
        .into_response()
}

#[derive(Deserialize)]
struct ItemScope {
    item_id: String,
}

async fn pm_comments_handler(Query(q): Query<ItemScope>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::dashboard::data::comments_json(&q.item_id),
    )
        .into_response()
}

#[derive(Deserialize)]
struct LabelScope {
    workspace_id: Option<String>,
    project_id: Option<String>,
}

async fn pm_labels_handler(Query(q): Query<LabelScope>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::dashboard::data::labels_json(q.workspace_id.as_deref(), q.project_id.as_deref()),
    )
        .into_response()
}

async fn webhooks_handler(Query(q): Query<WorkspaceScope>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::dashboard::data::webhooks_json(&q.workspace_id),
    )
        .into_response()
}

#[derive(Deserialize)]
struct CostQuery {
    days: Option<u32>,
    by: Option<String>,
}

async fn cost_handler(Query(q): Query<CostQuery>) -> Response {
    let days = q.days.unwrap_or(1);
    let by = q.by.as_deref().unwrap_or("model");
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::dashboard::data::cost_json(days, by),
    )
        .into_response()
}

/// `/events` cadence. Claims are a cheap indexed SQLite read, so they refresh
/// every `TICK`. The cost summary is the expensive surface — `cost::summarize`
/// walks the whole `~/.claude/projects` tree and rewrites the analytics cache —
/// so it is recomputed only every `COST_REFRESH` and cached between. With the
/// "skip while nobody's watching" guard, an idle or single-tab dashboard costs
/// almost nothing, and N tabs still cost just one refresh cycle.
const TICK: std::time::Duration = std::time::Duration::from_secs(3);
const COST_REFRESH: std::time::Duration = std::time::Duration::from_secs(30);

/// Single shared broadcast of the live `{ claims, cost_today }` snapshot. Every
/// `/events` client subscribes to this one channel, so there is no per-client
/// work — the producer below runs at most one refresh cycle per `TICK`,
/// regardless of how many browser tabs are connected.
fn snapshot_broadcaster() -> tokio::sync::broadcast::Sender<String> {
    use std::sync::OnceLock;
    static TX: OnceLock<tokio::sync::broadcast::Sender<String>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, _rx) = tokio::sync::broadcast::channel::<String>(4);
        let producer = tx.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(TICK);
            // The expensive cost summary is refreshed on its own slower
            // schedule and cached here between refreshes.
            let mut cost_today = String::from("{}");
            let mut cost_age = COST_REFRESH; // force a refresh on the first tick
            loop {
                ticker.tick().await;
                // Nobody's listening → do no work at all; no idle disk churn.
                if producer.receiver_count() == 0 {
                    continue;
                }
                // Recompute the cost summary only once its cache has aged past
                // COST_REFRESH; the walk+cache-write is kept off the async
                // worker threads via spawn_blocking.
                if cost_age >= COST_REFRESH {
                    cost_today = tokio::task::spawn_blocking(|| {
                        crate::dashboard::data::cost_json(1, "model")
                    })
                    .await
                    .unwrap_or_else(|_| "{}".to_string());
                    cost_age = std::time::Duration::ZERO;
                }
                cost_age += TICK;
                // Claims are cheap (one indexed read) — refresh every tick,
                // still off the worker threads.
                let claims = tokio::task::spawn_blocking(crate::dashboard::data::claims_json)
                    .await
                    .unwrap_or_else(|_| "[]".to_string());
                // Err only means every receiver dropped mid-cycle; the next
                // tick no-ops via receiver_count.
                let _ = producer.send(crate::dashboard::data::snapshot_json(&claims, &cost_today));
            }
        });
        tx
    })
    .clone()
}

/// Server-Sent Events stream of the volatile surfaces (claims + today's cost).
/// Subscribes to the shared broadcast, so N connected tabs cost one refresh
/// cycle rather than N. A newly connected client waits up to one `TICK` (~3s)
/// for its first frame; the view shows "connecting…" until then.
async fn events_handler()
-> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let rx = snapshot_broadcaster().subscribe();
    // Drop lagged/errored frames — the next snapshot supersedes them.
    let stream = tokio_stream::StreamExt::filter_map(
        tokio_stream::wrappers::BroadcastStream::new(rx),
        |msg| match msg {
            Ok(data) => Some(Ok(Event::default().data(data))),
            Err(_) => None,
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match WebAssets::get(path) {
        Some(c) => ([(header::CONTENT_TYPE, mime_for(path))], c.data.into_owned()).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn mime_for(p: &str) -> &'static str {
    if p.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if p.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if p.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

pub fn router() -> Router {
    Router::new()
        .route("/api/claims", get(claims_handler))
        .route("/api/pm/workspaces", get(pm_workspaces_handler))
        .route("/api/pm/projects", get(pm_projects_handler))
        .route("/api/pm/items", get(pm_items_handler))
        .route("/api/pm/states", get(pm_states_handler))
        .route("/api/pm/comments", get(pm_comments_handler))
        .route("/api/pm/labels", get(pm_labels_handler))
        .route("/api/webhooks", get(webhooks_handler))
        .route("/api/cost", get(cost_handler))
        .route("/events", get(events_handler))
        .fallback(static_handler)
}

pub async fn run(host: &str, port: u16, open: bool) {
    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .expect("failed to bind dashboard server");
    let addr = listener.local_addr().expect("no local addr");
    let url = format!("http://{addr}");
    eprintln!("agentflare dashboard listening on {url}");
    if host != "127.0.0.1" && host != "localhost" {
        eprintln!("  warning: bound to {host} — anyone on your network can view this");
    }
    if open {
        crate::dashboard::open_browser(&url);
    }
    axum::serve(listener, router()).await.expect("dashboard server error");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn claims_endpoint_returns_json_array() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router()).await.unwrap(); });
        let body = reqwest::get(format!("http://{addr}/api/claims"))
            .await.unwrap().text().await.unwrap();
        assert!(body.starts_with('['), "expected JSON array, got: {body}");
    }
}
