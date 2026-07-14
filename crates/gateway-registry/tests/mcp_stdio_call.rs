//! Integration tests for `McpStdioBackend::call()` against the real
//! `gateway-fixture-server` binary, spoken over the real `rmcp` client
//! transport (spawned child process + stdio).
//!
//! These live here (rather than as a `#[cfg(test)] mod tests` inside
//! `src/mcp_stdio.rs`) for the same reason as `tests/mcp_stdio_discover.rs`:
//! `env!("CARGO_BIN_EXE_gateway-fixture-server")` is only populated by Cargo
//! for integration test / benchmark targets, not for the lib's own
//! unit-test binary — see the note in `src/mcp_stdio.rs`.

use agentflare_gateway_registry::{GatewayError, McpStdioBackend};
use std::collections::HashMap;

fn fixture_path() -> String {
    env!("CARGO_BIN_EXE_gateway-fixture-server").to_string()
}

#[tokio::test]
async fn call_echo_returns_the_downstream_result() {
    let backend = McpStdioBackend::new(fixture_path(), vec![], HashMap::new());
    let result = backend
        .call("echo", serde_json::json!({"text": "hi"}))
        .await
        .unwrap();
    let text = result
        .get(0)
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str());
    assert_eq!(text, Some("echo: hi"));
}

#[tokio::test]
async fn call_unknown_tool_surfaces_as_upstream_error() {
    let backend = McpStdioBackend::new(fixture_path(), vec![], HashMap::new());
    let err = backend
        .call("does_not_exist", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, GatewayError::Upstream(_)));
}

#[tokio::test]
async fn call_with_non_object_args_is_invalid_argument_not_upstream() {
    // Malformed `args` (not a JSON object or null) is rejected entirely
    // locally, before any downstream I/O — a caller mistake, not a
    // downstream/infrastructure failure, so it must be `InvalidArgument`
    // (which `tool_execute` maps to `invalid_params`), not `Upstream`
    // (which maps to `internal_error`).
    let backend = McpStdioBackend::new(fixture_path(), vec![], HashMap::new());
    let err = backend
        .call("echo", serde_json::json!("not an object"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, GatewayError::InvalidArgument(_)),
        "expected InvalidArgument, got {err:?}"
    );
}
