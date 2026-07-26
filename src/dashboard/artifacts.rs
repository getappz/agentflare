use agentflare_artifacts::ArtifactStore;
use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response, Sse},
    routing::get,
};
use serde::Deserialize;
use std::sync::Arc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;

#[derive(Clone)]
struct ArtifactState {
    store: Arc<ArtifactStore>,
    base_url: String,
}

fn open_store() -> ArtifactState {
    let store = match crate::store::open() {
        Ok(s) => ArtifactStore::with_store(s),
        Err(e) => {
            eprintln!("[dashboard/artifacts] failed to open store: {e}");
            ArtifactStore::new(crate::paths::home().join(".agentflare").join("artifacts"))
        }
    };
    ArtifactState {
        store: Arc::new(store),
        base_url: "/artifacts".to_string(),
    }
}

async fn index(State(state): State<ArtifactState>) -> Response {
    let html = tokio::task::spawn_blocking(move || {
        agentflare_artifacts::render_index(&state.store, &state.base_url)
    })
    .await
    .unwrap_or_else(|_| "error".into());
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

#[derive(Deserialize)]
struct VersionPath {
    id: String,
    version: u32,
}

async fn artifact_page(State(state): State<ArtifactState>, Path(id): Path<String>) -> Response {
    let Ok(artifact) = state.store.get(&id) else {
        return (StatusCode::NOT_FOUND, "artifact not found").into_response();
    };
    let html = agentflare_artifacts::render_artifact_page(&artifact, true, &state.base_url);
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

async fn artifact_version_page(
    State(state): State<ArtifactState>,
    Path(VersionPath { id, version }): Path<VersionPath>,
) -> Response {
    let Ok(artifact) = state.store.get_version(&id, version) else {
        return (StatusCode::NOT_FOUND, "artifact version not found").into_response();
    };
    let html = agentflare_artifacts::render_artifact_page(&artifact, false, &state.base_url);
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

async fn versions_json(State(state): State<ArtifactState>, Path(id): Path<String>) -> Response {
    match state.store.versions(&id) {
        Ok(history) => (
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_string_pretty(&history).unwrap_or_else(|_| "[]".into()),
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "artifact not found").into_response(),
    }
}

async fn artifact_live(State(state): State<ArtifactState>, Path(id): Path<String>) -> Response {
    if !agentflare_artifacts::valid_id(&id) {
        return (StatusCode::NOT_FOUND, "invalid id").into_response();
    }
    let rx = state.store.subscribe(&id);
    let (tx, async_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::task::spawn_blocking(move || {
        while let Ok(event) = rx.recv() {
            if tx.send(event).is_err() {
                break;
            }
        }
    });
    let stream = UnboundedReceiverStream::new(async_rx).map(|event| {
        Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(event))
    });
    Sse::new(stream).into_response()
}

pub fn router() -> Router {
    let state = open_store();
    Router::new()
        .route("/", get(index))
        .route("/{id}", get(artifact_page))
        .route("/{id}/v/{version}", get(artifact_version_page))
        .route("/{id}/versions", get(versions_json))
        .route("/{id}/live", get(artifact_live))
        .with_state(state)
}
