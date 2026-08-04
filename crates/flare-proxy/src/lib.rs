mod forward;
pub mod heuristic;
pub mod providers;
pub mod shape_xlat;
pub mod shortcircuit;
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
            config: ProviderConfig::from_env(),
            sc_config: shortcircuit::ShortCircuitConfig::from_env(),
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
    match shortcircuit::try_short_circuit(&body, &state.sc_config) {
        shortcircuit::ShortCircuitOutcome::Match(message) => {
            // Internal CLI bookkeeping calls (quota probe, prefix/title/
            // suggestion/filepath detection) are sent non-streaming and
            // expect a plain Messages JSON body, unlike every other request
            // this proxy handles.
            return axum::Json(message).into_response();
        }
        shortcircuit::ShortCircuitOutcome::NoMatch => {}
    }
    forward::proxy_request(body, &state.config, &state.client).await
}

#[derive(Clone)]
struct AppState {
    config: ProviderConfig,
    sc_config: shortcircuit::ShortCircuitConfig,
    client: reqwest::Client,
}
