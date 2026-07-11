//! Proves a service-level error (`Ok(Err(_))` from rmcp's `call_tool`) also
//! clears the cached connection, not just a timeout (`mcp_stdio_timeout.rs`
//! already covers that path). rmcp doesn't distinguish "downstream server
//! returned a clean protocol error" from "the pipe died underneath us" at
//! this call site, so both are treated the same: drop the cache, let the
//! next call/discover respawn.
//!
//! Same marker-file technique as `mcp_stdio_timeout.rs`: the fixture appends
//! its PID on every process start, so counting lines proves a *new* child
//! process was actually spawned, not just that some later call happened to
//! succeed.

use agentflare_gateway_registry::{GatewayError, McpStdioBackend};
use std::collections::HashMap;
use std::time::Duration;

#[tokio::test]
async fn service_error_on_a_cached_connection_clears_it_so_the_next_call_reconnects() {
    let marker = tempfile::NamedTempFile::new().unwrap();
    let marker_path = marker.path().to_path_buf();
    let fixture = env!("CARGO_BIN_EXE_gateway-fixture-server").to_string();
    let mut env = HashMap::new();
    env.insert(
        "GATEWAY_FIXTURE_MARKER_FILE".to_string(),
        marker_path.to_string_lossy().to_string(),
    );
    let backend = McpStdioBackend::with_timeout(fixture, vec![], env, Duration::from_secs(5));

    // First call connects (spawn #1) and succeeds normally, populating the cache.
    backend
        .call("echo", serde_json::json!({"text": "hi"}))
        .await
        .unwrap();

    // Second call reuses the cached connection but the fixture's "fail" tool
    // returns a service-level error, not a timeout.
    let err = backend
        .call("fail", serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, GatewayError::Upstream(_)),
        "expected Upstream, got {err:?}"
    );

    // Third call must NOT reuse the connection from before the error — it
    // must respawn a fresh child process, same as the timeout case.
    backend
        .call("echo", serde_json::json!({"text": "again"}))
        .await
        .unwrap();

    let marker_contents = std::fs::read_to_string(&marker_path).unwrap();
    let spawn_count = marker_contents.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(
        spawn_count, 2,
        "expected exactly 2 child-process spawns (initial connect + post-error \
         reconnect), got {spawn_count}: {marker_contents:?}"
    );
}

#[tokio::test]
async fn concurrent_calls_to_the_same_backend_never_panic() {
    let fixture = env!("CARGO_BIN_EXE_gateway-fixture-server").to_string();
    let backend = std::sync::Arc::new(agentflare_gateway_registry::McpStdioBackend::new(
        fixture,
        vec![],
        HashMap::new(),
    ));
    let calls = (0..20).map(|i| {
        let backend = backend.clone();
        async move {
            backend
                .call("echo", serde_json::json!({"text": i.to_string()}))
                .await
        }
    });
    let results = futures_util::future::join_all(calls).await;
    assert!(
        results.iter().all(|r| r.is_ok()),
        "expected all concurrent calls to succeed: {results:?}"
    );
}
