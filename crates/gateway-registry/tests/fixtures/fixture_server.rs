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
    let service = FixtureServer.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
