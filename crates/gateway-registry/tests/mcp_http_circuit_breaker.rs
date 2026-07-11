//! Proves `McpHttpBackend` opens its circuit after repeated consecutive
//! failures, mirroring `tests/mcp_stdio_circuit_breaker.rs` exactly (both
//! backends share the same `CircuitBreaker`, extracted in Task 1).

use agentflare_gateway_registry::{GatewayError, McpHttpBackend};
use std::time::Duration;

fn broken_backend(circuit_recovery: Duration) -> McpHttpBackend {
    McpHttpBackend::with_timeout_and_circuit_recovery(
        "http://127.0.0.1:1/mcp".to_string(),
        None,
        Duration::from_secs(5),
        circuit_recovery,
    )
}

#[tokio::test]
async fn circuit_opens_after_threshold_consecutive_failures() {
    let backend = broken_backend(Duration::from_secs(60));
    for _ in 0..3 {
        let err = backend
            .call("echo", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, GatewayError::Connection(_)),
            "expected Connection, got {err:?}"
        );
    }
    let err = backend
        .call("echo", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, GatewayError::CircuitOpen(_)),
        "expected CircuitOpen, got {err:?}"
    );
}

#[tokio::test]
async fn circuit_allows_one_probe_after_recovery_window() {
    let backend = broken_backend(Duration::from_millis(200));
    for _ in 0..3 {
        backend
            .call("echo", serde_json::json!({}))
            .await
            .unwrap_err();
    }
    let err = backend
        .call("echo", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, GatewayError::CircuitOpen(_)),
        "expected CircuitOpen, got {err:?}"
    );

    tokio::time::sleep(Duration::from_millis(250)).await;

    let err = backend
        .call("echo", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, GatewayError::Connection(_)),
        "expected a real probe attempt, got {err:?}"
    );
}
