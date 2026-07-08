//! Integration test proving `McpStdioBackend` bounds downstream I/O with a
//! timeout instead of hanging forever (final-review Finding 1: no timeout
//! anywhere in this crate meant one hung backend could wedge the whole
//! `Registry`, since `ensure_gateway_registry` holds a single lock across
//! every backend's calls).
//!
//! Uses the `gateway-fixture-server` fixture's "hang" mode
//! (`GATEWAY_FIXTURE_HANG=1`) to simulate an unresponsive downstream server:
//! the fixture never completes the MCP `initialize` handshake, so both
//! `discover()` and `call()` block on `ensure_connected()` until our
//! short test-only timeout fires — proving the mechanism without waiting out
//! the crate's real 30s `DEFAULT_TIMEOUT`.

use agentflare_gateway_registry::{GatewayError, McpStdioBackend};
use std::collections::HashMap;
use std::time::Duration;

fn hung_backend(timeout: Duration) -> McpStdioBackend {
    let fixture = env!("CARGO_BIN_EXE_gateway-fixture-server").to_string();
    let mut env = HashMap::new();
    env.insert("GATEWAY_FIXTURE_HANG".to_string(), "1".to_string());
    McpStdioBackend::with_timeout(fixture, vec![], env, timeout)
}

#[tokio::test]
async fn discover_against_a_hung_backend_times_out_instead_of_hanging_forever() {
    let backend = hung_backend(Duration::from_secs(1));
    let err = backend.discover().await.unwrap_err();
    assert!(matches!(err, GatewayError::Timeout(_)), "expected Timeout, got {err:?}");
}

#[tokio::test]
async fn call_against_a_hung_backend_times_out_instead_of_hanging_forever() {
    let backend = hung_backend(Duration::from_secs(1));
    let err = backend.call("echo", serde_json::json!({"text": "hi"})).await.unwrap_err();
    assert!(matches!(err, GatewayError::Timeout(_)), "expected Timeout, got {err:?}");
}

/// Fix 1 (final review): a timed-out `call()`/`discover()` must clear the
/// cached `RunningService` so the NEXT call reconnects, instead of
/// `ensure_connected` seeing `guard.is_some()` and reusing a connection to
/// what may now be a permanently wedged child process.
///
/// Unlike the two tests above (which hang before `initialize` even
/// completes, so no connection is ever cached), this test connects
/// successfully first, then times out on a specific tool call (`hang`,
/// which never returns) against the ALREADY-cached connection, then proves
/// the NEXT call spawns a brand-new child process rather than reusing the
/// old one — via a marker file the fixture appends its PID to on every
/// process start.
#[tokio::test]
async fn timeout_on_a_cached_connection_clears_it_so_the_next_call_reconnects() {
    let marker = tempfile::NamedTempFile::new().unwrap();
    let marker_path = marker.path().to_path_buf();
    let fixture = env!("CARGO_BIN_EXE_gateway-fixture-server").to_string();
    let mut env = HashMap::new();
    env.insert("GATEWAY_FIXTURE_MARKER_FILE".to_string(), marker_path.to_string_lossy().to_string());
    let backend = McpStdioBackend::with_timeout(fixture, vec![], env, Duration::from_millis(500));

    // First call connects (spawn #1) and succeeds normally, populating the cache.
    backend.call("echo", serde_json::json!({"text": "hi"})).await.unwrap();

    // Second call reuses the cached connection but hangs forever on the
    // "hang" tool, so it must time out.
    let err = backend.call("hang", serde_json::json!({})).await.unwrap_err();
    assert!(matches!(err, GatewayError::Timeout(_)), "expected Timeout, got {err:?}");

    // Third call must NOT reuse the (potentially wedged) cached connection
    // from before the timeout — it must respawn a fresh child process.
    backend.call("echo", serde_json::json!({"text": "again"})).await.unwrap();

    let marker_contents = std::fs::read_to_string(&marker_path).unwrap();
    let spawn_count = marker_contents.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(
        spawn_count, 2,
        "expected exactly 2 child-process spawns (initial connect + post-timeout \
         reconnect), got {spawn_count}: {marker_contents:?}"
    );
}
