//! Shared in-process HTTP MCP fixture server, used by every `mcp_http_*`
//! integration test. Unlike the stdio fixture (`gateway-fixture-server`,
//! spawned as a separate child-process binary — required because stdio
//! transport needs a distinct process to pipe to), an HTTP MCP server can
//! run as a plain background task in the SAME test process: no separate
//! `[[bin]]` target or `CARGO_BIN_EXE_*` plumbing needed.
//!
//! Exposes the same three tools as `gateway-fixture-server` (`echo`,
//! `hang`, `fail`) so HTTP tests can mirror the existing stdio tests
//! call-for-call. Not itself a test file — Cargo only auto-discovers `.rs`
//! files directly under `tests/`, not files in a subdirectory, so this is
//! pulled in via `mod support;` from each real test file instead.

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{Implementation, ServerCapabilities, ServerInfo},
    schemars,
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EchoRequest {
    #[schemars(description = "Text to echo back")]
    text: String,
}

#[derive(Clone, Default)]
struct HttpFixtureServer;

#[tool_router]
impl HttpFixtureServer {
    #[tool(description = "Echoes the given text back, prefixed with 'echo: '.")]
    fn echo(&self, Parameters(EchoRequest { text }): Parameters<EchoRequest>) -> String {
        format!("echo: {text}")
    }

    #[tool(description = "Never returns; simulates a wedged downstream tool call.")]
    async fn hang(&self) -> String {
        std::future::pending::<()>().await;
        unreachable!("pending future never resolves")
    }

    #[tool(description = "Always errors; simulates a downstream tool-level failure.")]
    fn fail(&self) -> Result<String, rmcp::ErrorData> {
        Err(rmcp::ErrorData::invalid_params("simulated failure", None))
    }
}

#[tool_handler]
impl ServerHandler for HttpFixtureServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("gateway-fixture-http-server", "0.1.0"))
    }
}

/// A running fixture server. Dropping this cancels the background server
/// task — every test's fixture is torn down automatically at scope exit,
/// no manual shutdown call needed.
pub struct HttpFixture {
    pub url: String,
    cancel: CancellationToken,
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Starts the fixture server on an OS-assigned localhost port and returns
/// its full MCP endpoint URL (e.g. `http://127.0.0.1:54321/mcp`).
pub async fn start() -> HttpFixture {
    let ct = CancellationToken::new();
    let service: StreamableHttpService<HttpFixtureServer, LocalSessionManager> = StreamableHttpService::new(
        || Ok(HttpFixtureServer),
        Default::default(),
        StreamableHttpServerConfig::default()
            .with_sse_keep_alive(None)
            .with_cancellation_token(ct.child_token()),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind fixture http listener");
    let addr = listener.local_addr().expect("fixture listener local_addr");

    let server_ct = ct.clone();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async move { server_ct.cancelled_owned().await })
            .await;
    });

    HttpFixture { url: format!("http://{addr}/mcp"), cancel: ct }
}
