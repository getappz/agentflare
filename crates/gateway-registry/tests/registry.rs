//! Integration tests for `Registry` end-to-end against the real
//! `gateway-fixture-server` binary, spoken over the real `rmcp` client
//! transport (spawned child process + stdio).
//!
//! These live here (rather than as a `#[cfg(test)] mod tests` inside
//! `src/registry.rs`) for the same reason as `tests/mcp_stdio_discover.rs`
//! and `tests/mcp_stdio_call.rs`: `env!("CARGO_BIN_EXE_gateway-fixture-server")`
//! is only populated by Cargo for integration test / benchmark targets, not
//! for the lib's own unit-test binary — see the note at the bottom of
//! `src/registry.rs`.

mod support;

use agentflare_gateway_registry::{GatewayConfig, GatewayError, MatchMode, Registry, ServerConfig};
use std::collections::HashMap;

fn fixture_path() -> String {
    env!("CARGO_BIN_EXE_gateway-fixture-server").to_string()
}

fn config_with_fixture() -> GatewayConfig {
    let mut servers = HashMap::new();
    servers.insert(
        "fixture".to_string(),
        ServerConfig::McpStdio {
            command: fixture_path(),
            args: vec![],
            auth_ref: None,
            auth_env: None,
        },
    );
    GatewayConfig { servers }
}

#[tokio::test]
async fn search_finds_the_fixture_tool_after_open() {
    let reg = Registry::open_in_memory(&config_with_fixture(), &HashMap::new())
        .await
        .unwrap();
    let hits = reg.search("echo", 5, MatchMode::All).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].tool, "echo");
    assert_eq!(hits[0].server, "fixture");
}

#[tokio::test]
async fn execute_dispatches_to_the_right_backend() {
    let reg = Registry::open_in_memory(&config_with_fixture(), &HashMap::new())
        .await
        .unwrap();
    let result = reg
        .execute("fixture", "echo", serde_json::json!({"text": "hi"}))
        .await
        .unwrap();
    let text = result
        .get(0)
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str());
    assert_eq!(text, Some("echo: hi"));
}

/// End-to-end: a `kind = "mcp_http"` server config, resolved through
/// `build_backends` into a real `Backend::McpHttp`, searchable and
/// executable through `Registry` exactly like an `mcp_stdio` backend above.
#[tokio::test]
async fn execute_dispatches_to_an_mcp_http_backend() {
    let fixture = support::start().await;
    let mut servers = HashMap::new();
    servers.insert(
        "http_fixture".to_string(),
        ServerConfig::McpHttp {
            url: fixture.url.clone(),
            auth_ref: None,
            auth_env: None,
            auth_header: None,
        },
    );
    let config = GatewayConfig { servers };

    let reg = Registry::open_in_memory(&config, &HashMap::new())
        .await
        .unwrap();

    let hits = reg.search("echo", 5, MatchMode::All).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].server, "http_fixture");

    let result = reg
        .execute("http_fixture", "echo", serde_json::json!({"text": "hi"}))
        .await
        .unwrap();
    let text = result
        .get(0)
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str());
    assert_eq!(text, Some("echo: hi"));
}

#[tokio::test]
async fn execute_unknown_server_suggests_the_closest_name() {
    let reg = Registry::open_in_memory(&config_with_fixture(), &HashMap::new())
        .await
        .unwrap();
    let err = reg
        .execute("fixtur", "echo", serde_json::json!({}))
        .await
        .unwrap_err();
    match err {
        GatewayError::ServerNotFound(msg) => assert!(msg.contains("fixture")),
        other => panic!("expected ServerNotFound, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_unknown_tool_suggests_the_closest_name() {
    let reg = Registry::open_in_memory(&config_with_fixture(), &HashMap::new())
        .await
        .unwrap();
    let err = reg
        .execute("fixture", "ech", serde_json::json!({}))
        .await
        .unwrap_err();
    match err {
        GatewayError::ToolNotFound(msg) => assert!(msg.contains("echo")),
        other => panic!("expected ToolNotFound, got {other:?}"),
    }
}

/// One healthy `mcp_stdio` backend alongside one deliberately-broken
/// `mcp_stdio` backend pointed at a nonexistent binary (its `discover()`
/// fails immediately on spawn). The healthy backend's tools must still be
/// indexed, searchable, and executable — a single backend's discovery
/// failure must not poison the whole registry's refresh.
#[tokio::test]
async fn one_failing_backend_does_not_block_the_others() {
    let mut servers = HashMap::new();
    servers.insert(
        "fixture".to_string(),
        ServerConfig::McpStdio {
            command: fixture_path(),
            args: vec![],
            auth_ref: None,
            auth_env: None,
        },
    );
    servers.insert(
        "broken".to_string(),
        ServerConfig::McpStdio {
            command: "definitely-not-a-real-binary-xyz".to_string(),
            args: vec![],
            auth_ref: None,
            auth_env: None,
        },
    );
    let config = GatewayConfig { servers };

    let reg = Registry::open_in_memory(&config, &HashMap::new())
        .await
        .unwrap();

    let hits = reg.search("echo", 5, MatchMode::All).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].tool, "echo");
    assert_eq!(hits[0].server, "fixture");

    let result = reg
        .execute("fixture", "echo", serde_json::json!({"text": "hi"}))
        .await
        .unwrap();
    let text = result
        .get(0)
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str());
    assert_eq!(text, Some("echo: hi"));
}
