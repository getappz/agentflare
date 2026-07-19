mod forward;
pub mod heuristic;
pub mod providers;
pub mod shape_xlat;
pub mod think;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
pub use providers::ProviderConfig;

pub fn router() -> Router {
    Router::new()
        .route("/proxy/v1/messages", post(v1_messages_handler))
        .with_state(AppState {
            config: ProviderConfig::default_free(),
        })
}

/// When `AGENTFLARE_PROXY_TOKEN` is set, requests must carry a matching
/// `x-agentflare-proxy-token` header. This route forwards to paid/free
/// upstream APIs using server-held credentials and is mounted on the
/// dashboard server, which can be bound off-localhost — without this gate
/// anyone reachable on the network could spend the operator's provider quota.
async fn v1_messages_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Json(body): axum::extract::Json<serde_json::Value>,
) -> Response {
    if let Ok(expected) = std::env::var("AGENTFLARE_PROXY_TOKEN") {
        let provided = headers
            .get("x-agentflare-proxy-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != expected {
            return (StatusCode::UNAUTHORIZED, "invalid or missing proxy token").into_response();
        }
    }
    forward::proxy_request(body, &state.config).await
}

#[derive(Clone)]
struct AppState {
    config: ProviderConfig,
}
