//! Integration tests for `McpHttpBackend::call()` against the in-process
//! HTTP MCP fixture server (`tests/support`).

mod support;

use agentflare_gateway_registry::{GatewayError, McpHttpBackend};

#[tokio::test]
async fn call_echo_returns_the_downstream_result() {
    let fixture = support::start().await;
    let backend = McpHttpBackend::new(fixture.url.clone(), None);
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
    let fixture = support::start().await;
    let backend = McpHttpBackend::new(fixture.url.clone(), None);
    let err = backend
        .call("does_not_exist", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(matches!(err, GatewayError::Upstream(_)));
}

#[tokio::test]
async fn call_with_non_object_args_is_invalid_argument_not_upstream() {
    let fixture = support::start().await;
    let backend = McpHttpBackend::new(fixture.url.clone(), None);
    let err = backend
        .call("echo", serde_json::json!("not an object"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, GatewayError::InvalidArgument(_)),
        "expected InvalidArgument, got {err:?}"
    );
}

#[tokio::test]
async fn call_against_unreachable_url_is_a_connection_error() {
    // Port 1 on localhost: reserved, nothing listens there, connection is
    // refused immediately — same "always fails, no real wait" property
    // `tests/mcp_stdio_circuit_breaker.rs` gets from a nonexistent binary.
    let backend = McpHttpBackend::new("http://127.0.0.1:1/mcp".to_string(), None);
    let err = backend
        .call("echo", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, GatewayError::Connection(_)),
        "expected Connection, got {err:?}"
    );
}
