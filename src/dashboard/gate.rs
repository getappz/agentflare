//! Host resource dispatch gate status/reset endpoints (item #643).
//!
//! Split out of `server.rs` to keep that file under the LOC gate
//! (`scripts/loc-gate.sh`), same reasoning as `chat.rs`. `GET /api/gate`
//! reports the gate's current policy; `POST /api/gate/reset`
//! force-unpauses a gate stuck on `AGENTFLARE_DISPATCH_GATE_MODE=off`
//! baked into the daemon's environment — `agentflare daemon restart`
//! alone doesn't clear it, since restart just re-reads the same stuck env
//! var. Reachable from the CLI (`agentflare daemon gate reset`) over HTTP
//! because the gate's state lives in this process, not the CLI's.

use axum::{
    Router,
    http::header,
    response::{IntoResponse, Response},
    routing::{get, post},
};

pub fn router() -> Router {
    Router::new()
        .route("/api/gate", get(gate_status_handler))
        .route("/api/gate/reset", post(gate_reset_handler))
}

fn gate_json_response() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        crate::dashboard::data::gate_status_json(),
    )
        .into_response()
}

async fn gate_status_handler() -> Response {
    gate_json_response()
}

async fn gate_reset_handler() -> Response {
    agentflare_resource_gate::force_resume();
    gate_json_response()
}
