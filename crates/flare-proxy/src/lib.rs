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
use std::time::Duration;

pub fn router() -> Router {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .unwrap_or_default();
    Router::new()
        .route("/proxy/v1/messages", post(v1_messages_handler))
        .with_state(AppState {
            config: ProviderConfig::default_free(),
            client,
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
        if expected.is_empty() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "AGENTFLARE_PROXY_TOKEN is set but empty",
            )
                .into_response();
        }
        let provided = headers
            .get("x-agentflare-proxy-token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != expected {
            return (StatusCode::UNAUTHORIZED, "invalid or missing proxy token").into_response();
        }
    }
    forward::proxy_request(body, &state.config, &state.client).await
}

#[derive(Clone)]
struct AppState {
    config: ProviderConfig,
    client: reqwest::Client,
}
