//! Localhost HTTP surface. Never binds beyond 127.0.0.1.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::config::Config;
use crate::daemon::{make_verified_killer, sweep_once, unix_now, SharedSnapshot, SweepOutcome};
use crate::events::EventLog;
use crate::leases::LeaseStore;
use crate::model::{Finding, Identity, Lease, ProcInfo};
use crate::scanner::identity_of;

type HttpError = (StatusCode, String);

fn internal(err: impl std::fmt::Display) -> HttpError {
    (StatusCode::INTERNAL_SERVER_ERROR, err.to_string())
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub snapshot: SharedSnapshot,
    pub store: Arc<LeaseStore>,
    pub log: Arc<EventLog>,
    /// Per-install secret required on every mutating request. Guards the
    /// kill-enabled endpoints against localhost CSRF: a web page can fire a
    /// cross-origin POST at 127.0.0.1, but it cannot read this token, and the
    /// custom header forces a CORS preflight that never passes.
    pub token: Arc<String>,
    /// Shared agentflare artifact store served under /artifacts; None when
    /// disabled in config.
    pub artifacts: Option<Arc<agentflare_artifacts::ArtifactStore>>,
}

impl AppState {
    pub fn new(cfg: Arc<Config>, state_dir: PathBuf) -> Self {
        let artifacts = cfg.artifacts_enabled.then(|| {
            Arc::new(agentflare_artifacts::ArtifactStore::new(cfg.artifacts_dir.clone()))
        });
        Self {
            cfg,
            snapshot: SharedSnapshot::default(),
            store: Arc::new(LeaseStore::new(&state_dir)),
            log: Arc::new(EventLog::new(&state_dir)),
            token: Arc::new(load_or_create_token(&state_dir)),
            artifacts,
        }
    }
}

fn load_or_create_token(dir: &std::path::Path) -> String {
    use std::hash::BuildHasher;
    let path = dir.join("token");
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let existing = existing.trim().to_string();
        if !existing.is_empty() {
            return existing;
        }
    }
    // 128 bits from std's randomly-keyed SipHash (RandomState seeds from OS
    // entropy) mixed with time and pid.
    let mut token = String::with_capacity(32);
    for round in 0u8..2 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let word = std::collections::hash_map::RandomState::new()
            .hash_one((nanos, std::process::id(), round));
        token.push_str(&format!("{word:016x}"));
    }
    let _ = std::fs::create_dir_all(dir);
    if let Err(err) = std::fs::write(&path, &token) {
        tracing::warn!(%err, "could not persist API token; using session-only token");
    }
    token
}

async fn require_token(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let mutating = matches!(req.method().as_str(), "POST" | "DELETE" | "PUT" | "PATCH");
    if mutating {
        let ok = req
            .headers()
            .get("x-flared-token")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == s.token.as_str());
        if !ok {
            return (
                StatusCode::UNAUTHORIZED,
                "missing or invalid x-flared-token (value lives in flared's state dir)",
            )
                .into_response();
        }
    }
    next.run(req).await
}

#[derive(Deserialize)]
pub struct LeaseRequest {
    pub pid: u32,
    #[serde(default = "default_class")]
    pub class: String,
    pub ttl_seconds: u64,
    #[serde(default)]
    pub allow_kill: bool,
}

fn default_class() -> String {
    "agent".into()
}

#[derive(Deserialize)]
pub struct EventsQuery {
    #[serde(default = "default_events_n")]
    pub n: usize,
}

fn default_events_n() -> usize {
    50
}

pub fn router(state: AppState) -> Router {
    let artifacts = state.artifacts.clone();
    let router = Router::new()
        .route("/", get(dashboard))
        .route("/status", get(status))
        .route("/processes", get(processes))
        .route("/orphans", get(orphans))
        .route("/events", get(events))
        .route("/leases", get(leases_list))
        .route("/lease", post(lease_create))
        .route("/lease/{id}/heartbeat", post(lease_heartbeat))
        .route("/lease/{id}", delete(lease_delete))
        .route("/sweep", post(sweep_now))
        .route("/clean", post(clean_dry_run))
        .route("/clean/execute", post(clean_execute))
        .layer(middleware::from_fn_with_state(state.clone(), require_token))
        .with_state(state);
    match artifacts {
        // Merged outside the token layer: the artifact routes are read-only
        // GETs, and the guard only gates mutating methods anyway.
        Some(store) => router.merge(crate::artifacts::router(store)),
        None => router,
    }
}

async fn dashboard() -> Html<&'static str> {
    Html(DASHBOARD)
}

async fn status(State(s): State<AppState>) -> Json<serde_json::Value> {
    let last = s.snapshot.lock().expect("snapshot lock").last.clone();
    let lease_count = s.store.load().map(|l| l.len()).unwrap_or(0);
    Json(serde_json::json!({
        "last": last,
        "lease_count": lease_count,
        "port": s.cfg.port,
    }))
}

async fn processes(State(s): State<AppState>) -> Json<Vec<ProcInfo>> {
    Json(s.snapshot.lock().expect("snapshot lock").processes.clone())
}

async fn orphans(State(s): State<AppState>) -> Json<Vec<Finding>> {
    let snap = s.snapshot.lock().expect("snapshot lock");
    Json(snap.last.as_ref().map(|l| l.orphans.clone()).unwrap_or_default())
}

async fn events(
    State(s): State<AppState>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<Vec<serde_json::Value>>, HttpError> {
    s.log.tail(q.n).map(Json).map_err(internal)
}

async fn leases_list(State(s): State<AppState>) -> Result<Json<Vec<Lease>>, HttpError> {
    s.store.load().map(Json).map_err(internal)
}

async fn lease_create(
    State(s): State<AppState>,
    Json(req): Json<LeaseRequest>,
) -> Result<Json<Lease>, HttpError> {
    let pid = req.pid;
    let identity = tokio::task::spawn_blocking(move || identity_of(pid))
        .await
        .map_err(internal)?
        .ok_or((StatusCode::NOT_FOUND, format!("pid {pid} is not running")))?;
    let lease = s
        .store
        .create(
            pid,
            &req.class,
            req.ttl_seconds,
            Identity { exe_name: identity.0, start_time: identity.1 },
            req.allow_kill,
            unix_now(),
        )
        .map_err(internal)?;
    let _ = s.log.append(
        "lease.create",
        serde_json::json!({ "id": lease.id, "pid": lease.pid, "class": lease.class }),
    );
    Ok(Json(lease))
}

async fn lease_heartbeat(
    State(s): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Lease>, HttpError> {
    match s.store.heartbeat(&id, unix_now()).map_err(internal)? {
        Some(lease) => Ok(Json(lease)),
        None => Err((StatusCode::NOT_FOUND, format!("unknown lease '{id}'"))),
    }
}

async fn lease_delete(
    State(s): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, HttpError> {
    let removed = s.store.remove(&id).map_err(internal)?;
    let _ = s.log.append("lease.delete", serde_json::json!({ "id": id, "removed": removed }));
    Ok(Json(serde_json::json!({ "removed": removed })))
}

async fn run_sweep(s: AppState, execute: bool) -> Result<Json<SweepOutcome>, HttpError> {
    let worker = s.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        sweep_once(
            &worker.cfg,
            &worker.store,
            &worker.log,
            execute,
            true,
            &mut make_verified_killer(&worker.store),
        )
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    // Publish so /status, /orphans, and the dashboard see manual sweeps too.
    s.snapshot.lock().expect("snapshot lock").last = Some(outcome.clone());
    Ok(Json(outcome))
}

async fn sweep_now(State(s): State<AppState>) -> Result<Json<SweepOutcome>, HttpError> {
    run_sweep(s, true).await
}

async fn clean_dry_run(State(s): State<AppState>) -> Result<Json<SweepOutcome>, HttpError> {
    run_sweep(s, false).await
}

async fn clean_execute(State(s): State<AppState>) -> Result<Json<SweepOutcome>, HttpError> {
    run_sweep(s, true).await
}

const DASHBOARD: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>flared</title>
<style>
body{font-family:ui-monospace,monospace;margin:2rem;background:#111;color:#ddd}
h1{color:#f80}table{border-collapse:collapse;margin-top:1rem}
td,th{padding:.25rem .75rem;border-bottom:1px solid #333;text-align:left}
.green{color:#6c6}.yellow{color:#cc6}.red{color:#c66}
</style></head><body>
<h1>flared</h1>
<div id="pressure">loading…</div>
<h2>workloads</h2><table id="buckets"></table>
<h2>orphan findings</h2><table id="orphans"></table>
<script>
// All dynamic values go through textContent — process names and finding
// reasons are attacker-influenced strings and must never reach innerHTML.
function row(cells){
  const tr=document.createElement('tr');
  for(const c of cells){const td=document.createElement('td');td.textContent=String(c);tr.appendChild(td);}
  return tr;
}
async function tick(){
  const s = await (await fetch('/status')).json();
  const last = s.last;
  const pressure = document.getElementById('pressure');
  if(!last){pressure.textContent='no sweep yet';return}
  const p = last.pressure;
  const level=document.createElement('span');
  level.className=p.level; level.textContent=p.level;
  pressure.replaceChildren('pressure: ', level,
    ` · cpu ${p.cpu_pct.toFixed(0)}%` +
    ` · mem free ${(p.avail_mem_bytes/2**30).toFixed(1)} GiB` +
    ` · leases ${s.lease_count}`);
  document.getElementById('buckets').replaceChildren(
    ...Object.entries(last.bucket_counts).sort((a,b)=>b[1]-a[1]).map(([k,v])=>row([k,v])));
  const orphans = await (await fetch('/orphans')).json();
  document.getElementById('orphans').replaceChildren(
    ...(orphans.length ? orphans.map(o=>row([o.pid,o.name,o.reason])) : [row(['none'])]));
}
tick(); setInterval(tick, 15000);
</script></body></html>"#;

/// Serve until aborted. Binds 127.0.0.1 only.
pub async fn serve(state: AppState) -> eyre::Result<()> {
    let port = state.cfg.port;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(%port, "flared http listening on 127.0.0.1");
    axum::serve(listener, router(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            // keep artifact serving inside the temp dir, not the real store
            artifacts_dir: dir.path().join("artifacts-store"),
            ..Config::default()
        };
        let state = AppState::new(Arc::new(cfg), dir.path().to_path_buf());
        (dir, state)
    }

    async fn body_json(response: axum::response::Response) -> serde_json::Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn artifact_routes_are_mounted_and_need_no_token() {
        let (_dir, state) = state();
        let app = router(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/artifacts")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(
            String::from_utf8_lossy(&bytes).contains("agentflare artifacts"),
            "artifact index must serve under /artifacts"
        );
    }

    #[tokio::test]
    async fn artifact_routes_absent_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config {
            artifacts_enabled: false,
            artifacts_dir: dir.path().join("artifacts-store"),
            ..Config::default()
        };
        let state = AppState::new(Arc::new(cfg), dir.path().to_path_buf());
        let app = router(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/artifacts")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn status_serves_snapshot_json() {
        let (_dir, state) = state();
        let app = router(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/status")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert!(json.get("last").is_some());
        assert!(json.get("lease_count").is_some());
    }

    #[tokio::test]
    async fn dashboard_serves_html() {
        let (_dir, state) = state();
        let app = router(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn lease_lifecycle_over_http() {
        let (_dir, state) = state();
        let app = router(state.clone());

        // Register a lease for OUR OWN live process — identity is resolvable.
        let me = std::process::id();
        let token = state.token.as_str().to_string();
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/lease")
                    .header("content-type", "application/json")
                    .header("x-flared-token", &token)
                    .body(axum::body::Body::from(
                        serde_json::json!({ "pid": me, "ttl_seconds": 300 }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let lease = body_json(response).await;
        let id = lease["id"].as_str().unwrap().to_string();
        assert_eq!(lease["pid"].as_u64().unwrap() as u32, me);
        assert!(!lease["identity"]["exe_name"].as_str().unwrap().is_empty());

        // Heartbeat.
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/lease/{id}/heartbeat"))
                    .header("x-flared-token", &token)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Delete.
        let response = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("DELETE")
                    .uri(format!("/lease/{id}"))
                    .header("x-flared-token", &token)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(state.store.load().unwrap().is_empty());
    }

    #[tokio::test]
    async fn lease_for_dead_pid_is_rejected() {
        let (_dir, state) = state();
        let token = state.token.as_str().to_string();
        let app = router(state);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/lease")
                    .header("content-type", "application/json")
                    .header("x-flared-token", &token)
                    .body(axum::body::Body::from(
                        serde_json::json!({ "pid": u32::MAX - 7, "ttl_seconds": 300 })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn mutating_request_without_token_is_unauthorized() {
        let (_dir, state) = state();
        let app = router(state.clone());
        for (method, uri) in
            [("POST", "/sweep"), ("POST", "/clean/execute"), ("DELETE", "/lease/x")]
        {
            let response = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method(method)
                        .uri(uri)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri} must require the token"
            );
        }
        // Reads stay open.
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/status")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
