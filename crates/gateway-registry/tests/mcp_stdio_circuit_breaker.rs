//! Proves `McpStdioBackend` opens its circuit after repeated consecutive
//! failures, short-circuiting further calls without attempting a spawn,
//! then allows exactly one probe attempt through once the recovery window
//! passes.
//!
//! Uses a command that can never spawn (`definitely-not-a-real-binary-xyz`)
//! so every attempt fails immediately with `GatewayError::Connection` —
//! no need for the fixture server here.

use agentflare_gateway_registry::{GatewayError, McpStdioBackend};
use std::collections::HashMap;
use std::time::Duration;

fn broken_backend(circuit_recovery: Duration) -> McpStdioBackend {
    McpStdioBackend::with_timeout_and_circuit_recovery(
        "definitely-not-a-real-binary-xyz".to_string(),
        vec![],
        HashMap::new(),
        Duration::from_secs(5),
        circuit_recovery,
    )
}

#[tokio::test]
async fn circuit_opens_after_threshold_consecutive_failures() {
    let backend = broken_backend(Duration::from_secs(60));

    // First 3 failures are genuine spawn-connection failures.
    for _ in 0..3 {
        let err = backend.call("echo", serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, GatewayError::Connection(_)), "expected Connection, got {err:?}");
    }

    // 4th call must short-circuit as CircuitOpen instead of attempting
    // another spawn.
    let err = backend.call("echo", serde_json::json!({})).await.unwrap_err();
    assert!(matches!(err, GatewayError::CircuitOpen(_)), "expected CircuitOpen, got {err:?}");
}

#[tokio::test]
async fn circuit_allows_one_probe_after_recovery_window() {
    let backend = broken_backend(Duration::from_millis(200));

    for _ in 0..3 {
        backend.call("echo", serde_json::json!({})).await.unwrap_err();
    }
    let err = backend.call("echo", serde_json::json!({})).await.unwrap_err();
    assert!(matches!(err, GatewayError::CircuitOpen(_)), "expected CircuitOpen, got {err:?}");

    tokio::time::sleep(Duration::from_millis(250)).await;

    // Past the recovery window, the next call must be a real probe attempt
    // (Connection failure again, since the binary still doesn't exist) —
    // not another fast-failed CircuitOpen.
    let err = backend.call("echo", serde_json::json!({})).await.unwrap_err();
    assert!(matches!(err, GatewayError::Connection(_)), "expected a real probe attempt, got {err:?}");
}

#[tokio::test]
async fn a_success_resets_the_failure_count() {
    let fixture = env!("CARGO_BIN_EXE_gateway-fixture-server").to_string();
    let backend = McpStdioBackend::with_timeout_and_circuit_recovery(
        fixture,
        vec![],
        HashMap::new(),
        Duration::from_secs(5),
        Duration::from_secs(60),
    );

    // Two real failures (unknown tool — a service-level error, not enough
    // on its own to open a 3-failure circuit)...
    backend.call("no-such-tool", serde_json::json!({})).await.unwrap_err();
    backend.call("no-such-tool", serde_json::json!({})).await.unwrap_err();

    // ...then a success must reset the counter, so two MORE failures still
    // don't open the circuit.
    backend.call("echo", serde_json::json!({"text": "hi"})).await.unwrap();
    backend.call("no-such-tool", serde_json::json!({})).await.unwrap_err();
    let err = backend.call("no-such-tool", serde_json::json!({})).await.unwrap_err();
    assert!(matches!(err, GatewayError::Upstream(_)), "expected Upstream (circuit still closed), got {err:?}");
}
