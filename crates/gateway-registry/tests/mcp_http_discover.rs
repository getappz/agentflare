//! Integration tests for `McpHttpBackend::discover()` against the in-process
//! HTTP MCP fixture server (`tests/support`), spoken over the real `rmcp`
//! Streamable-HTTP client transport.

mod support;

use agentflare_gateway_registry::McpHttpBackend;

#[tokio::test]
async fn discover_lists_the_fixture_servers_echo_tool() {
    let fixture = support::start().await;
    let backend = McpHttpBackend::new(fixture.url.clone(), None);
    let tools = backend.discover().await.unwrap();
    let echo = tools
        .iter()
        .find(|t| t.name == "echo")
        .expect("echo tool present");
    assert!(echo.description.contains("Echoes"));
}

#[tokio::test]
async fn discover_is_idempotent_reusing_the_connection() {
    let fixture = support::start().await;
    let backend = McpHttpBackend::new(fixture.url.clone(), None);
    let first = backend.discover().await.unwrap();
    let second = backend.discover().await.unwrap();
    assert_eq!(first.len(), second.len());
}
