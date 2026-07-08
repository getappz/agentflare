//! Real MCP client over a spawned child process, using `rmcp`'s own client
//! transport (`TokioChildProcess` + the `initialize` handshake via
//! `ServiceExt::serve`) — not a bespoke REST bridge.

use crate::error::GatewayError;
use crate::types::ToolEntry;
use rmcp::{
    service::RunningService,
    transport::{ConfigureCommandExt, TokioChildProcess},
    RoleClient, ServiceExt,
};
use serde_json::Value;
use std::collections::HashMap;

pub struct McpStdioBackend {
    running: tokio::sync::Mutex<Option<RunningService<RoleClient, ()>>>,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
}

impl McpStdioBackend {
    pub fn new(command: String, args: Vec<String>, env: HashMap<String, String>) -> Self {
        Self { running: tokio::sync::Mutex::new(None), command, args, env }
    }

    async fn ensure_connected(&self) -> Result<(), GatewayError> {
        let mut guard = self.running.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let transport = TokioChildProcess::new(tokio::process::Command::new(&self.command).configure(
            |cmd| {
                cmd.args(&self.args);
                for (k, v) in &self.env {
                    cmd.env(k, v);
                }
            },
        ))
        .map_err(|e| GatewayError::Connection(format!("spawn '{}' failed: {e}", self.command)))?;
        let running = ().serve(transport).await.map_err(|e| {
            GatewayError::Connection(format!("connect to '{}' failed: {e}", self.command))
        })?;
        *guard = Some(running);
        Ok(())
    }

    pub async fn discover(&self) -> Result<Vec<ToolEntry>, GatewayError> {
        self.ensure_connected().await?;
        let guard = self.running.lock().await;
        let running = guard.as_ref().expect("connected above");
        let tools = running
            .list_all_tools()
            .await
            .map_err(|e| GatewayError::Upstream(e.to_string()))?;
        Ok(tools
            .into_iter()
            .map(|t| ToolEntry {
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: Value::Object((*t.input_schema).clone()),
            })
            .collect())
    }

    pub async fn call(&self, _tool: &str, _args: Value) -> Result<Value, GatewayError> {
        todo!("implemented in Task 7")
    }
}

// NOTE: the discover()-against-the-real-fixture-binary tests originally
// planned as a `#[cfg(test)] mod tests` here had to move to
// `tests/mcp_stdio_discover.rs` instead. `env!("CARGO_BIN_EXE_<name>")` is
// only populated by Cargo when compiling an *integration* test target (a
// file under `tests/`) or a benchmark — it is not set when compiling the
// lib's own unit-test binary (`cargo test --lib`), so `cargo build` failed
// with "environment variable `CARGO_BIN_EXE_gateway-fixture-server` not
// defined at compile time" when the tests lived here. Moving them to an
// integration test (which only needs the public `McpStdioBackend`/`discover`
// API this module already exports) fixes that with no change to the
// non-test code above.
