//! Artifact routes: serve the shared agentflare artifact store under
//! `/artifacts` on flared's HTTP port, so artifact URLs survive individual
//! agent sessions and other agents can fetch handoffs with no session open.
//!
//! Read-only by design: publishing stays with the MCP tools, which write
//! to the store directory directly. GET routes bypass the token guard.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use agentflare_artifacts::{render_artifact_page, render_index, valid_id, ArtifactStore};
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::{Stream, StreamExt};

/// URL prefix the artifact routes are mounted under; baked into rendered
/// pages' internal links.
pub const ROUTE_PREFIX: &str = "/artifacts";

pub fn router(store: Arc<ArtifactStore>) -> Router {
    Router::new()
        .route("/artifacts", get(index))
        .route("/artifacts/", get(index))
        .route("/artifacts/{id}", get(latest_page))
        .route("/artifacts/{id}/v/{version}", get(version_page))
        .route("/artifacts/{id}/versions", get(versions))
        .route("/artifacts/{id}/live", get(live))
        .with_state(store)
}

async fn index(State(store): State<Arc<ArtifactStore>>) -> Html<String> {
    Html(render_index(&store, ROUTE_PREFIX))
}

async fn latest_page(
    State(store): State<Arc<ArtifactStore>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Html<String>, StatusCode> {
    page(&store, &id, None)
}

async fn version_page(
    State(store): State<Arc<ArtifactStore>>,
    AxumPath((id, version)): AxumPath<(String, u32)>,
) -> Result<Html<String>, StatusCode> {
    page(&store, &id, Some(version))
}

fn page(store: &ArtifactStore, id: &str, version: Option<u32>) -> Result<Html<String>, StatusCode> {
    if !valid_id(id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let artifact = match version {
        Some(v) => store.get_version(id, v),
        None => store.get(id),
    }
    .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Html(render_artifact_page(&artifact, version.is_none(), ROUTE_PREFIX)))
}

async fn versions(
    State(store): State<Arc<ArtifactStore>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !valid_id(&id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let history = store.versions(&id).map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::to_value(history).unwrap_or_default()))
}

/// Live-reload stream. This process never publishes (the MCP tools do,
/// from their own processes), so there is no broadcast channel to listen
/// on — poll the disk store and emit an event when the artifact changes.
async fn live(
    State(store): State<Arc<ArtifactStore>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, StatusCode> {
    if !valid_id(&id) {
        return Err(StatusCode::NOT_FOUND);
    }
    let mut last = store.get(&id).ok().map(|a| (a.version, a.updated_at));
    let ticks = IntervalStream::new(tokio::time::interval(Duration::from_secs(2)));
    let stream = ticks.filter_map(move |_| {
        let now = store.get(&id).ok().map(|a| (a.version, a.updated_at));
        if now.is_some() && now != last {
            last = now;
            Some(Ok(Event::default().event("update").data("update")))
        } else {
            None
        }
    });
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentflare_artifacts::PublishRequest;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn served() -> (tempfile::TempDir, Arc<ArtifactStore>, Router) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ArtifactStore::new(dir.path().to_path_buf()));
        let router = router(store.clone());
        (dir, store, router)
    }

    async fn get_page(app: Router, uri: &str) -> (StatusCode, String) {
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri(uri)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).to_string())
    }

    fn publish(store: &ArtifactStore, name: &str, content: &str) -> String {
        store
            .publish(&PublishRequest {
                name: name.into(),
                content: content.into(),
                session_id: "s".into(),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    #[tokio::test]
    async fn index_lists_artifacts_with_prefixed_links() {
        let (_dir, store, app) = served();
        let id = publish(&store, "my-doc", "hello");
        let (status, body) = get_page(app, "/artifacts").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("agentflare artifacts"), "{body}");
        assert!(body.contains("my-doc"), "{body}");
        assert!(body.contains(&format!("href=\"/artifacts/{id}\"")), "{body}");
    }

    #[tokio::test]
    async fn page_serves_content_and_prefixed_live_reload() {
        let (_dir, store, app) = served();
        let id = publish(&store, "doc", "PAGE-CONTENT");
        let (status, body) = get_page(app, &format!("/artifacts/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("PAGE-CONTENT"), "{body}");
        assert!(body.contains(&format!("/artifacts/{id}/live")), "{body}");
    }

    #[tokio::test]
    async fn versions_and_snapshots_serve() {
        let (_dir, store, app) = served();
        let id = publish(&store, "doc", "OLD-CONTENT");
        store
            .publish(&PublishRequest {
                name: "doc".into(),
                content: "NEW-CONTENT".into(),
                session_id: "s".into(),
                update_id: Some(id.clone()),
                ..Default::default()
            })
            .unwrap();
        let (status, body) = get_page(app.clone(), &format!("/artifacts/{id}/v/1")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("OLD-CONTENT"), "{body}");
        assert!(body.contains(&format!("href=\"/artifacts/{id}\"")), "snapshot banner links back under the prefix: {body}");
        let (status, body) = get_page(app, &format!("/artifacts/{id}/versions")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"version\""), "{body}");
    }

    #[tokio::test]
    async fn unknown_and_invalid_ids_404() {
        let (_dir, _store, app) = served();
        let (status, _) = get_page(app.clone(), "/artifacts/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // URL-encoded traversal decodes to a multi-segment id — rejected
        let (status, _) = get_page(app, "/artifacts/..%2F..%2Fescape").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
