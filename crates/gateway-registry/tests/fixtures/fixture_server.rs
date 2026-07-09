//! Minimal MCP server, spawned as a child process by gateway-registry's
//! own integration tests to exercise the real rmcp client transport
//! end-to-end. Exposes one tool: `echo`.

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ServerHandler, ServiceExt,
};
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EchoRequest {
    #[schemars(description = "Text to echo back")]
    text: String,
}

#[derive(Clone, Default)]
struct FixtureServer;

#[tool_router]
impl FixtureServer {
    #[tool(description = "Echoes the given text back, prefixed with 'echo: '.")]
    fn echo(&self, Parameters(EchoRequest { text }): Parameters<EchoRequest>) -> String {
        format!("echo: {text}")
    }

    /// Never returns — simulates one specific downstream tool call wedging
    /// forever (as opposed to `GATEWAY_FIXTURE_HANG`, which wedges the
    /// whole connection before `initialize`). Used by
    /// `tests/mcp_stdio_timeout.rs` to prove a timed-out `call()`/`discover()`
    /// clears the cached connection instead of letting the next call reuse
    /// it (Fix 1).
    #[tool(description = "Never returns; simulates a wedged downstream tool call.")]
    async fn hang(&self) -> String {
        std::future::pending::<()>().await;
        unreachable!("pending future never resolves")
    }

    /// Returns immediately with a service-level error (not a timeout) —
    /// simulates a downstream tool call that fails cleanly rather than the
    /// connection wedging. Used by `tests/mcp_stdio_reconnect.rs` to prove a
    /// `Ok(Err(_))` from `call_tool` also clears the cached connection,
    /// since rmcp doesn't distinguish "server said no" from "pipe died" at
    /// that call site.
    #[tool(description = "Always errors; simulates a downstream tool-level failure.")]
    fn fail(&self) -> Result<String, rmcp::ErrorData> {
        Err(rmcp::ErrorData::invalid_params("simulated failure", None))
    }
}

#[tool_handler]
impl ServerHandler for FixtureServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("gateway-fixture-server", "0.1.0"))
    }
}

// current_thread: the shared `tokio` dependency enables only `rt` (not
// `rt-multi-thread`), which is all gateway-registry's own async code needs;
// the default `#[tokio::main]` flavor requires `rt-multi-thread`, so this
// fixture-only binary picks the flavor that works with the existing feature
// set instead of widening it crate-wide for a test fixture.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // GATEWAY_FIXTURE_MARKER_FILE: if set, append one line to this file on
    // every process start. Lets `tests/mcp_stdio_timeout.rs` prove that a
    // *new* child process was actually spawned after a timeout (Fix 1),
    // rather than just that some later call happened to succeed.
    if let Ok(marker_path) = std::env::var("GATEWAY_FIXTURE_MARKER_FILE") {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(marker_path) {
            let _ = writeln!(f, "{}", std::process::id());
        }
    }
    // GATEWAY_FIXTURE_HANG simulates a hung/unresponsive downstream MCP
    // server for gateway-registry's timeout tests: never completes the
    // `initialize` handshake (never even calls `serve`), so a client talking
    // to this process sees exactly what it'd see from a wedged real backend.
    if std::env::var("GATEWAY_FIXTURE_HANG").is_ok() {
        std::future::pending::<()>().await;
    }
    let service = FixtureServer.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
