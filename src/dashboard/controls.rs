//! Dashboard job/run controls: `POST /api/jobs/:id/cancel` and
//! `POST /api/items/:id/{cancel,pause,resume,redispatch}`. Each is a thin
//! wrapper over `crate::job_controls`, the same code the `agentflare job` /
//! `agentflare item` CLI, the MCP `item` tool and the chat commands run.
//! The optional JSON body carries `reason` (cancel/pause) and `agent`
//! (redispatch).

use axum::{
    Json, Router,
    body::Bytes,
    extract::Path,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};

#[derive(Default, serde::Deserialize)]
struct ControlBody {
    reason: Option<String>,
    agent: Option<String>,
}

/// An empty body is fine; a malformed one is a caller error.
fn parse_body(body: &Bytes) -> Result<ControlBody, String> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(ControlBody::default());
    }
    serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))
}

/// Runs a blocking control off the async workers (engine calls and SQLite
/// writes both block) and maps its result onto a JSON response.
async fn run_control(
    f: impl FnOnce() -> Result<serde_json::Value, String> + Send + 'static,
) -> Response {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => (StatusCode::BAD_REQUEST, e).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("control task failed: {e}"),
        )
            .into_response(),
    }
}

async fn cancel_job_handler(Path(id): Path<String>) -> Response {
    run_control(move || crate::job_controls::cancel_job(&id)).await
}

async fn item_control_handler(Path((id, action)): Path<(String, String)>, body: Bytes) -> Response {
    if !matches!(
        action.as_str(),
        "cancel" | "pause" | "resume" | "redispatch"
    ) {
        return (
            StatusCode::NOT_FOUND,
            format!("unknown item control '{action}'"),
        )
            .into_response();
    }
    let body = match parse_body(&body) {
        Ok(body) => body,
        Err(e) => return (StatusCode::BAD_REQUEST, e).into_response(),
    };
    run_control(move || {
        let json = crate::job_controls::item_action(&id, &action, body.reason, body.agent)?;
        serde_json::from_str(&json).map_err(|e| e.to_string())
    })
    .await
}

pub(super) fn router() -> Router {
    Router::new()
        .route("/api/jobs/{id}/cancel", post(cancel_job_handler))
        .route("/api/items/{id}/{action}", post(item_control_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_body_parses_to_defaults_and_bad_json_is_rejected() {
        let body = parse_body(&Bytes::from_static(b"  ")).ok().unwrap();
        assert!(body.reason.is_none() && body.agent.is_none());
        let body = parse_body(&Bytes::from_static(br#"{"agent":"codex"}"#))
            .ok()
            .unwrap();
        assert_eq!(body.agent.as_deref(), Some("codex"));
        assert!(parse_body(&Bytes::from_static(b"{nope")).is_err());
    }
}
