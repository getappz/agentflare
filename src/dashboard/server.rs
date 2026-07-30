use agentflare_jobs::{AgentJob, JobState, Queue};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use std::io::Read;
use rust_embed::RustEmbed;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(RustEmbed)]
#[folder = "dashboard/web/"]
struct WebAssets;

async fn claims_handler() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::dashboard::data::claims_json(),
    )
        .into_response()
}

#[derive(Deserialize)]
struct WorkspaceScope {
    workspace_id: String,
}

async fn pm_workspaces_handler() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::dashboard::data::workspaces_json(),
    )
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
    let by = match q.by.as_deref() {
        None | Some("model") => "model",
        Some("project") => "project",
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid `by` value {other:?}; expected \"model\" or \"project\""),
            )
                .into_response();
        }
    };
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

/// How often to sweep finished jobs (and their stdout/stderr log files) out
/// of the queue, and how old a finished job must be before it's eligible.
/// Without this, `agent_jobs` rows and job-logs/ files accumulate forever —
/// nothing else ever calls `Queue::cleanup`.
const JOB_CLEANUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);
const JOB_RETENTION_SECS: i64 = 7 * 24 * 3600;

/// Runs for the lifetime of the process (like `snapshot_broadcaster`'s
/// producer task): wakes on `JOB_CLEANUP_INTERVAL`, deletes finished jobs
/// older than `JOB_RETENTION_SECS`. The blocking SQLite + filesystem work
/// happens off the async worker threads via `spawn_blocking`.
fn spawn_job_cleanup(queue: agentflare_jobs::Queue) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(JOB_CLEANUP_INTERVAL);
        loop {
            ticker.tick().await;
            let queue = queue.clone();
            let result =
                tokio::task::spawn_blocking(move || queue.cleanup(JOB_RETENTION_SECS)).await;
            match result {
                Ok(Ok(0)) => {}
                Ok(Ok(deleted)) => eprintln!("agentflare-jobs: cleaned up {deleted} old job(s)"),
                Ok(Err(e)) => eprintln!("agentflare-jobs: cleanup failed: {e}"),
                Err(e) => eprintln!("agentflare-jobs: cleanup task panicked: {e}"),
            }
        }
    });
}

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
        Some(c) => (
            [(header::CONTENT_TYPE, mime_for(path))],
            c.data.into_owned(),
        )
            .into_response(),
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

/// Body for `POST /api/jobs`. Only `command` is required; everything else
/// falls back to `AgentJob::new`'s defaults (300s timeout, 3 retries).
#[derive(Deserialize)]
struct SubmitJobRequest {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Vec<(String, String)>,
    cwd: Option<PathBuf>,
    timeout_secs: Option<u64>,
}

async fn submit_job_handler(
    State(queue): State<Queue>,
    Json(req): Json<SubmitJobRequest>,
) -> Response {
    if req.command.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "command must not be empty").into_response();
    }
    let mut job = AgentJob::new(req.command).args(req.args);
    for (k, v) in req.env {
        job = job.env(k, v);
    }
    if let Some(cwd) = req.cwd {
        job = job.cwd(cwd);
    }
    if let Some(secs) = req.timeout_secs {
        job = job.timeout(secs);
    }
    match queue.enqueue(&job) {
        Ok(info) => (StatusCode::CREATED, Json(info)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to enqueue job: {e}"),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct JobsQuery {
    id: Option<String>,
    state: Option<String>,
}

/// `GET /api/jobs?id=<id>` fetches one job; `GET /api/jobs[?state=<state>]`
/// lists the most recent 100, optionally filtered by state.
///
/// `queue.get`/`queue.list` hit SQLite synchronously, so both go through
/// `spawn_blocking` rather than running inline on the async request path —
/// matching the SSE handlers below, which do the same for identical calls.
async fn jobs_handler(State(queue): State<Queue>, Query(q): Query<JobsQuery>) -> Response {
    if let Some(id) = q.id {
        let got = tokio::task::spawn_blocking({
            let queue = queue.clone();
            move || queue.get(&id)
        })
        .await;
        return match got {
            Ok(Ok(info)) => Json(info).into_response(),
            Ok(Err(_)) => (StatusCode::NOT_FOUND, "job not found").into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("job lookup task failed: {e}"),
            )
                .into_response(),
        };
    }
    let state_filter = match q.state.as_deref() {
        None => None,
        Some("queued") => Some(JobState::Queued),
        Some("running") => Some(JobState::Running),
        Some("exited") => Some(JobState::Exited),
        Some("killed") => Some(JobState::Killed),
        Some("failed") => Some(JobState::Failed),
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid `state` value {other:?}"),
            )
                .into_response();
        }
    };
    let listed = tokio::task::spawn_blocking(move || queue.list(state_filter)).await;
    match listed {
        Ok(Ok(jobs)) => Json(jobs).into_response(),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to list jobs: {e}"),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("job list task failed: {e}"),
        )
            .into_response(),
    }
}

/// SSE stream of the live job list. Each subscriber independently polls
/// `queue.list(None)` on the same `TICK` cadence as the existing event
/// stream — cheap enough (SQLite LIMIT 100) that a shared broadcaster is
/// overkill for typical dashboard usage (1-2 tabs).
async fn jobs_events_handler(
    State(queue): State<Queue>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
        Result<Event, std::convert::Infallible>,
    >();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        loop {
            ticker.tick().await;
            let listed = tokio::task::spawn_blocking({
                let queue = queue.clone();
                move || queue.list(None)
            })
            .await;
            let list = match listed {
                Ok(Ok(list)) => list,
                Ok(Err(e)) => {
                    eprintln!("[dashboard/server] jobs_events: list failed: {e}");
                    vec![]
                }
                Err(e) => {
                    eprintln!("[dashboard/server] jobs_events: task failed: {e}");
                    vec![]
                }
            };
            let json = serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_string());
            if tx.send(Ok(Event::default().data(json))).is_err() {
                break;
            }
        }
    });
    Sse::new(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
}

/// `GET /api/jobs/:id/stream` — SSE stream that tails the job's stdout log
/// file incrementally.
///
/// The stdout path is derived directly from `queue.log_dir()` and the job's
/// own id (`Supervisor` names its log files `{id}.stdout`/`{id}.stderr` for
/// exactly this reason) rather than read from `JobInfo.output`, which is
/// only populated once the job reaches a terminal state — waiting on it
/// would mean this stream never emits anything until the job is already
/// finished, defeating the point of a *live* tail.
///
/// While the job is queued or running, we poll every 300ms: check the
/// current job state (`queue.get(id)`), read any new bytes from the stdout
/// file (which may not exist yet if the job is still queued), and push them
/// as SSE `data:` events. Once the job reaches a terminal state
/// (exited/failed/killed), any remaining buffered content is flushed, a
/// final `event: done\ndata:` frame is sent, and the stream closes.
async fn jobs_stream_handler(
    State(queue): State<Queue>,
    Path(id): Path<String>,
) -> Response {
    // 404 on unknown id before spawning anything.
    let exists = tokio::task::spawn_blocking({
        let queue = queue.clone();
        let id = id.clone();
        move || queue.get(&id).is_ok()
    })
    .await
    .unwrap_or(false);
    if !exists {
        return (StatusCode::NOT_FOUND, "job not found").into_response();
    }
    let stdout_path = queue.log_dir().join(format!("{id}.stdout"));
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<
        Result<Event, std::convert::Infallible>,
    >();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(300));
        let mut offset: u64 = 0;
        let mut file: Option<std::fs::File> = None;
        loop {
            interval.tick().await;
            let info = match tokio::task::spawn_blocking({
                let queue = queue.clone();
                let id = id.clone();
                move || queue.get(&id)
            })
            .await
            {
                Ok(Ok(info)) => info,
                _ => {
                    let _ = tx.send(Ok(Event::default().data("").event("done")));
                    break;
                }
            };
            let terminal = info.state.is_terminal();
            if file.is_none() {
                if stdout_path.exists() {
                    match std::fs::File::open(&stdout_path) {
                        Ok(f) => file = Some(f),
                        Err(_) => {
                            if terminal {
                                let _ = tx.send(Ok(Event::default().data("").event("done")));
                            }
                            break;
                        }
                    }
                }
                if file.is_none() {
                    if terminal {
                        let _ = tx.send(Ok(Event::default().data("").event("done")));
                        break;
                    }
                    continue;
                }
            }
            if let Some(ref mut f) = file {
                use std::io::Seek as _;
                let _ = f.seek(std::io::SeekFrom::Start(offset));
                let mut buf = Vec::new();
                match f.read_to_end(&mut buf) {
                    Ok(n) if n > 0 => {
                        offset += n as u64;
                        // Lossy, not strict: `buf` is a raw byte window that
                        // can split a multi-byte UTF-8 char across polls, and
                        // `offset` has already moved past it — a strict parse
                        // would silently and permanently drop those bytes
                        // from the live view instead of showing a stray
                        // replacement char at the boundary.
                        let text = String::from_utf8_lossy(&buf).into_owned();
                        if tx.send(Ok(Event::default().data(text))).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
            if terminal {
                let _ = tx.send(Ok(Event::default().data("").event("done")));
                break;
            }
        }
    });
    Sse::new(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Submit/fetch/stream endpoints as their own state-scoped sub-router.
fn jobs_router(queue: Queue) -> Router {
    Router::new()
        .route("/api/jobs", get(jobs_handler).post(submit_job_handler))
        .route("/api/jobs/events", get(jobs_events_handler))
        .route("/api/jobs/{id}/stream", get(jobs_stream_handler))
        .with_state(queue)
}

pub fn router(queue: Queue) -> Router {
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
        .merge(jobs_router(queue))
        .nest("/artifacts", super::artifacts::router())
        .merge(flare_proxy::router())
        .fallback(static_handler)
}

fn is_local_bind(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

pub async fn run(host: &str, port: u16, open: bool, yes_expose: bool) {
    if !is_local_bind(host) && !yes_expose {
        eprintln!(
            "refusing to bind to {host}: this would expose all PM/cost/webhook data with no authentication."
        );
        eprintln!("pass --yes-expose to bind anyway (trusted networks only).");
        std::process::exit(1);
    }
    // `agent_jobs` is relational state, so it lives in the single
    // source-of-truth `agentflare.db` (see `db.rs`'s header comment) rather
    // than a new file — this just opens its own `Connection` to that same
    // path and runs its own (non-overlapping) migration for that one table.
    // Job stdout/stderr logs are rebuildable artifacts, not DB state, so
    // those still get their own directory.
    let queue = Queue::open(
        &crate::db::agentflare_db_path(),
        crate::state::state_dir().join("job-logs"),
    )
    .expect("failed to open job queue");
    // Kept alive for the rest of `run()` (which never returns in normal
    // operation) so its worker threads keep polling the queue; there is no
    // graceful in-process shutdown path today (see `daemon::stop_daemon`,
    // which relies on SIGTERM/SIGKILL), so neither does this.
    let mut worker_pool = agentflare_jobs::WorkerPool::new(queue.clone());
    worker_pool.start(2);
    spawn_job_cleanup(queue.clone());

    let listener = tokio::net::TcpListener::bind((host, port))
        .await
        .expect("failed to bind dashboard server");
    let addr = listener.local_addr().expect("no local addr");
    let url = format!("http://{addr}");
    eprintln!("agentflare dashboard listening on {url}");
    if !is_local_bind(host) {
        eprintln!("  warning: bound to {host} — anyone on your network can view this");
    }
    if open {
        crate::dashboard::open_browser(&url);
    }
    axum::serve(listener, router(queue))
        .await
        .expect("dashboard server error");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_queue() -> Queue {
        // `.keep()` so the dir outlives this function — otherwise the
        // returned `Queue`'s `log_dir` would point at an already-deleted
        // path (harmless for tests that never touch logs, but a footgun for
        // ones that do).
        let dir = tempfile::tempdir().unwrap().keep();
        Queue::open_memory(dir.join("logs")).unwrap()
    }

    #[tokio::test]
    async fn claims_endpoint_returns_json_array() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(test_queue())).await.unwrap();
        });
        let body = reqwest::get(format!("http://{addr}/api/claims"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(body.starts_with('['), "expected JSON array, got: {body}");
    }

    #[tokio::test]
    async fn jobs_endpoint_round_trips_submit_and_fetch() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(test_queue())).await.unwrap();
        });

        let (cmd, args): (&str, Vec<&str>) = if cfg!(windows) {
            ("cmd", vec!["/c", "exit 0"])
        } else {
            ("true", vec![])
        };
        let client = reqwest::Client::new();
        let submit_resp = client
            .post(format!("http://{addr}/api/jobs"))
            .json(&serde_json::json!({ "command": cmd, "args": args }))
            .send()
            .await
            .unwrap();
        assert_eq!(submit_resp.status(), StatusCode::CREATED);
        let submitted: serde_json::Value = submit_resp.json().await.unwrap();
        assert_eq!(submitted["state"], "queued");
        assert_eq!(submitted["command"], cmd);
        let id = submitted["id"].as_str().unwrap().to_string();

        let fetch_resp = reqwest::get(format!("http://{addr}/api/jobs?id={id}"))
            .await
            .unwrap();
        assert_eq!(fetch_resp.status(), StatusCode::OK);
        let fetched: serde_json::Value = fetch_resp.json().await.unwrap();
        assert_eq!(fetched["id"], id);
        assert_eq!(
            fetched["command"], cmd,
            "list/get should surface the job's command, not just its id"
        );
    }

    #[tokio::test]
    async fn jobs_endpoint_rejects_empty_command() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(test_queue())).await.unwrap();
        });
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://{addr}/api/jobs"))
            .json(&serde_json::json!({ "command": "" }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn jobs_endpoint_404s_unknown_id() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(test_queue())).await.unwrap();
        });
        let resp = reqwest::get(format!("http://{addr}/api/jobs?id=does-not-exist"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn is_local_bind_recognizes_loopback_only() {
        assert!(is_local_bind("127.0.0.1"));
        assert!(is_local_bind("localhost"));
        assert!(is_local_bind("::1"));
        assert!(!is_local_bind("0.0.0.0"));
        assert!(!is_local_bind("192.168.1.5"));
    }

    #[test]
    fn mime_for_maps_known_extensions() {
        assert_eq!(mime_for("index.html"), "text/html; charset=utf-8");
        assert_eq!(mime_for("app.js"), "text/javascript; charset=utf-8");
        assert_eq!(mime_for("app.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("logo.png"), "application/octet-stream");
    }

    #[tokio::test]
    async fn static_handler_serves_index_at_root() {
        let resp = static_handler(Uri::from_static("/")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
    }

    #[tokio::test]
    async fn static_handler_404s_unknown_path() {
        let resp = static_handler(Uri::from_static("/does-not-exist.xyz")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn static_handler_404s_dot_dot_path_instead_of_escaping_assets() {
        // Embedded assets have no ".." keys, so a literal ".." in the path can
        // never escape `dashboard/web/` — it just misses the lookup.
        let resp = static_handler(Uri::from_static("/../Cargo.toml")).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn cost_handler_defaults_to_one_day_grouped_by_model() {
        let resp = cost_handler(Query(CostQuery {
            days: None,
            by: None,
        }))
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        // Live analytics data (this machine's own ~/.claude/projects usage)
        // can change between calls, so assert on shape rather than an exact
        // second snapshot — this still proves the days=1/by=model defaults
        // reached `cost_json` without a 400/500.
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v.get("groups").is_some(), "expected groups in {v}");
        assert!(
            v.get("total_cost_usd").is_some(),
            "expected total_cost_usd in {v}"
        );
        assert!(
            v.get("any_unpriced").is_some(),
            "expected any_unpriced in {v}"
        );
    }

    #[tokio::test]
    async fn cost_handler_rejects_unknown_by_value() {
        let resp = cost_handler(Query(CostQuery {
            days: None,
            by: Some("projct".into()),
        }))
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn jobs_events_endpoint_streams_job_list() {
        use tokio_stream::StreamExt as _;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let queue = test_queue();
        // Submit a job so the list isn't empty.
        let job = agentflare_jobs::AgentJob::new("true");
        queue.enqueue(&job).unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(queue)).await.unwrap();
        });
        let resp = reqwest::get(format!("http://{addr}/api/jobs/events"))
            .await
            .unwrap();
        let mut stream = resp.bytes_stream();
        let first = tokio::time::timeout(std::time::Duration::from_secs(10), stream.next())
            .await
            .expect("first SSE frame within 10s")
            .expect("stream item")
            .unwrap();
        let text = String::from_utf8(first.to_vec()).unwrap();
        let data_line = text
            .lines()
            .find_map(|l| l.strip_prefix("data: "))
            .expect("SSE data line");
        let v: serde_json::Value = serde_json::from_str(data_line).unwrap();
        assert!(v.is_array(), "expected JSON array, got: {v}");
        assert!(!v.as_array().unwrap().is_empty(), "expected at least one job");
    }

    #[tokio::test]
    async fn jobs_stream_endpoint_404s_unknown_id() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(test_queue())).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let resp = reqwest::get(format!("http://{addr}/api/jobs/does-not-exist/stream"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn jobs_stream_endpoint_streams_stdout_of_finished_job() {
        use tokio_stream::StreamExt as _;
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::open_memory(dir.path().join("logs")).unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let job = agentflare_jobs::AgentJob::new("echo").arg("hello from job");
        let info = queue.enqueue(&job).unwrap();
        let id = info.id.clone();
        let log_dir = queue.log_dir().to_path_buf();
        std::fs::create_dir_all(&log_dir).unwrap();
        let stdout_path = log_dir.join(format!("{id}.stdout"));
        std::fs::write(&stdout_path, b"hello from job\n").unwrap();
        let output = agentflare_jobs::JobOutput {
            exit_code: Some(0),
            timed_out: false,
            stdout_path: stdout_path.clone(),
            stderr_path: log_dir.join(format!("{id}.stderr")),
            stdout_total_bytes: 16,
            stderr_total_bytes: 0,
        };
        queue.complete(&id, &output, true).unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(queue)).await.unwrap();
        });
        // Give the server a moment to start.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let resp = reqwest::get(format!("http://{addr}/api/jobs/{id}/stream"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let mut stream = resp.bytes_stream();
        // Collect all SSE frames with a timeout.
        let mut all_text = String::new();
        loop {
            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                stream.next(),
            )
            .await
            {
                Ok(Some(Ok(chunk))) => {
                    all_text.push_str(&String::from_utf8_lossy(&chunk));
                    if all_text.contains("event: done") {
                        break;
                    }
                }
                Ok(Some(Err(_))) => break,
                Ok(None) => break,
                Err(_) => break,
            }
        }
        assert!(
            all_text.contains("hello from job"),
            "expected stdout content in stream, got: {all_text}"
        );
    }

    #[tokio::test]
    async fn jobs_stream_endpoint_tails_output_of_a_still_running_job() {
        // Regression: the stream used to key off `JobInfo.output`, which is
        // only populated once the job is already terminal — so it emitted
        // nothing at all until the job finished, then dumped everything in
        // one lump. This drives a real job through a real `WorkerPool` and
        // asserts the first chunk arrives (and the job is still `running`)
        // well before the job's own sleep would let it finish.
        use tokio_stream::StreamExt as _;
        let dir = tempfile::tempdir().unwrap();
        let queue = Queue::open_memory(dir.path().join("logs")).unwrap();
        let mut pool = agentflare_jobs::WorkerPool::new(queue.clone());
        pool.start(1);

        let (cmd, args): (&str, Vec<&str>) = if cfg!(windows) {
            ("cmd", vec!["/c", "echo tick1 & timeout /t 2 & echo tick2"])
        } else {
            ("sh", vec!["-c", "echo tick1; sleep 2; echo tick2"])
        };
        let info = queue.enqueue(&agentflare_jobs::AgentJob::new(cmd).args(args)).unwrap();
        let id = info.id.clone();

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let queue_for_router = queue.clone();
        tokio::spawn(async move {
            axum::serve(listener, router(queue_for_router)).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        let resp = reqwest::get(format!("http://{addr}/api/jobs/{id}/stream"))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let mut stream = resp.bytes_stream();

        let mut all_text = String::new();
        let mut state_at_first_tick: Option<String> = None;
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), stream.next()).await {
                Ok(Some(Ok(chunk))) => {
                    all_text.push_str(&String::from_utf8_lossy(&chunk));
                    if state_at_first_tick.is_none() && all_text.contains("tick1") {
                        let fetched: serde_json::Value =
                            reqwest::get(format!("http://{addr}/api/jobs?id={id}"))
                                .await
                                .unwrap()
                                .json()
                                .await
                                .unwrap();
                        state_at_first_tick =
                            Some(fetched["state"].as_str().unwrap_or("").to_string());
                    }
                    if all_text.contains("event: done") {
                        break;
                    }
                }
                Ok(Some(Err(_))) => break,
                Ok(None) => break,
                Err(_) => break,
            }
        }
        pool.shutdown();

        assert_eq!(
            state_at_first_tick.as_deref(),
            Some("running"),
            "expected the job to still be running when the first chunk arrived, got: {state_at_first_tick:?}"
        );
        assert!(
            all_text.contains("tick1") && all_text.contains("tick2"),
            "expected both ticks in stream, got: {all_text}"
        );
    }

    #[tokio::test]
    async fn events_endpoint_streams_claims_and_cost_snapshot() {
        use tokio_stream::StreamExt as _;
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, router(test_queue())).await.unwrap();
        });
        let resp = reqwest::get(format!("http://{addr}/events")).await.unwrap();
        let mut stream = resp.bytes_stream();
        let first = tokio::time::timeout(std::time::Duration::from_secs(10), stream.next())
            .await
            .expect("first SSE frame within 10s")
            .expect("stream item")
            .unwrap();
        let text = String::from_utf8(first.to_vec()).unwrap();
        let data_line = text
            .lines()
            .find_map(|l| l.strip_prefix("data: "))
            .expect("SSE data line");
        let v: serde_json::Value = serde_json::from_str(data_line).unwrap();
        assert!(v.get("claims").is_some(), "expected claims field in {v}");
        assert!(
            v.get("cost_today").is_some(),
            "expected cost_today field in {v}"
        );
    }

    // The PATH_LOCK guard below is intentionally held across `.await`
    // points: it must stay held for this whole test so no other test can
    // reset AGENTFLARE_HOME_OVERRIDE out from under it (a real race, not
    // just a lint nit — see the comment at the lock acquisition). Safe here
    // because `#[tokio::test]` gives each test its own current-thread
    // runtime on its own OS thread, so no other task ever contends for this
    // thread while the guard is held.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn run_serves_normally_on_local_bind_without_yes_expose() {
        // `run()` now opens a real, writable job queue under
        // `state::state_dir()` (`~/.agentflare` by default). Route it at a
        // throwaway home for the duration of this test — same lock +
        // env-var mechanism `paths::test_support::with_temp_home` uses, but
        // inlined because that helper's guard is sync-only and would reset
        // the override before the `.await`s below ever run.
        let _guard = agent_registry::detect::PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home_dir = tempfile::tempdir().unwrap();
        unsafe {
            // SAFETY: PATH_LOCK serializes all env mutation in this test binary.
            std::env::set_var("AGENTFLARE_HOME_OVERRIDE", home_dir.path());
        }

        // Probe an ephemeral port, then hand that exact port to `run()` —
        // `run()` takes a fixed port rather than returning the bound address,
        // so this is the only way to know where to connect afterward. Only
        // the local-bind path is exercised here; the non-local refusal path
        // calls `std::process::exit`, which isn't safe to trigger in-process
        // inside the test binary.
        let port = {
            let probe = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .unwrap();
            probe.local_addr().unwrap().port()
        };
        tokio::spawn(run("127.0.0.1", port, false, false));
        let mut started = false;
        for _ in 0..50 {
            if reqwest::get(format!("http://127.0.0.1:{port}/api/claims"))
                .await
                .is_ok()
            {
                started = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        unsafe {
            // SAFETY: still under PATH_LOCK.
            std::env::remove_var("AGENTFLARE_HOME_OVERRIDE");
        }

        assert!(
            started,
            "expected dashboard to serve on a local bind without --yes-expose"
        );
    }
}
