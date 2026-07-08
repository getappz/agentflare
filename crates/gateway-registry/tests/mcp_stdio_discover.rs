//! Integration tests for `McpStdioBackend::discover()` against the real
//! `gateway-fixture-server` binary, spoken over the real `rmcp` client
//! transport (spawned child process + stdio).
//!
//! These live here (rather than as a `#[cfg(test)] mod tests` inside
//! `src/mcp_stdio.rs`) because `env!("CARGO_BIN_EXE_gateway-fixture-server")`
//! is only populated by Cargo for integration test / benchmark targets, not
//! for the lib's own unit-test binary — see the note in `src/mcp_stdio.rs`.

use agentflare_gateway_registry::McpStdioBackend;
use std::collections::HashMap;

fn fixture_path() -> String {
    env!("CARGO_BIN_EXE_gateway-fixture-server").to_string()
}

#[tokio::test]
async fn discover_lists_the_fixture_servers_echo_tool() {
    let backend = McpStdioBackend::new(fixture_path(), vec![], HashMap::new());
    let tools = backend.discover().await.unwrap();
    // The fixture also exposes a `hang` tool (used by
    // `tests/mcp_stdio_timeout.rs` to test Fix 1's reconnect-after-timeout
    // behavior), so assert the `echo` tool specifically rather than the
    // exact total count.
    let echo = tools.iter().find(|t| t.name == "echo").expect("echo tool present");
    assert!(echo.description.contains("Echoes"));
}

#[tokio::test]
async fn discover_is_idempotent_reusing_the_connection() {
    let backend = McpStdioBackend::new(fixture_path(), vec![], HashMap::new());
    let first = backend.discover().await.unwrap();
    let second = backend.discover().await.unwrap();
    assert_eq!(first.len(), second.len());
}
