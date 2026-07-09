//! Real MCP client over a spawned child process, using `rmcp`'s own client
//! transport (`TokioChildProcess` + the `initialize` handshake via
//! `ServiceExt::serve`) — not a bespoke REST bridge.

use crate::circuit::{CircuitBreaker, CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_RECOVERY_TIMEOUT};
use crate::error::GatewayError;
use crate::types::ToolEntry;
use rmcp::{
    service::RunningService,
    transport::{ConfigureCommandExt, TokioChildProcess},
    RoleClient, ServiceExt,
};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// Bound on every downstream MCP round-trip (connect handshake, `discover`,
/// `call`) so one hung/slow backend can only ever fail after this long,
/// rather than wedging the whole `Registry` (which holds a single lock
/// guarding all backends — see `mcp_server.rs::ensure_gateway_registry`)
/// indefinitely. Not user-configurable in v1; 30s is long enough for a
/// legitimate slow tool call and short enough to bound the blast radius.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct McpStdioBackend {
    running: tokio::sync::Mutex<Option<RunningService<RoleClient, ()>>>,
    /// A persistently broken backend (missing binary, crashing on every
    /// spawn) would otherwise pay a full spawn + handshake + timeout cycle
    /// on every single call. After `CIRCUIT_FAILURE_THRESHOLD` consecutive
    /// failures, short-circuit for the recovery timeout instead of
    /// retrying — then allow exactly one probe attempt through.
    circuit: CircuitBreaker,
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
    timeout: Duration,
}

impl McpStdioBackend {
    pub fn new(command: String, args: Vec<String>, env: HashMap<String, String>) -> Self {
        Self::with_timeout(command, args, env, DEFAULT_TIMEOUT)
    }

    /// Same as [`Self::new`] but with an explicit timeout for downstream I/O.
    /// Exists mainly so tests can exercise the timeout path without waiting
    /// out the real `DEFAULT_TIMEOUT`; production callers should use `new`.
    pub fn with_timeout(
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        timeout: Duration,
    ) -> Self {
        Self::with_timeout_and_circuit_recovery(command, args, env, timeout, CIRCUIT_RECOVERY_TIMEOUT)
    }

    /// Same as [`Self::with_timeout`] but with an explicit circuit-breaker
    /// recovery window too. Exists mainly so tests can exercise the
    /// half-open recovery path without waiting out the real
    /// `CIRCUIT_RECOVERY_TIMEOUT`; production callers should use `new`.
    pub fn with_timeout_and_circuit_recovery(
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
        timeout: Duration,
        circuit_recovery: Duration,
    ) -> Self {
        Self {
            running: tokio::sync::Mutex::new(None),
            circuit: CircuitBreaker::new(CIRCUIT_FAILURE_THRESHOLD, circuit_recovery),
            command,
            args,
            env,
            timeout,
        }
    }

    async fn ensure_connected(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<RunningService<RoleClient, ()>>>, GatewayError> {
        let mut guard = self.running.lock().await;
        if guard.is_none() {
            let transport = TokioChildProcess::new(tokio::process::Command::new(&self.command).configure(
                |cmd| {
                    cmd.args(&self.args);
                    for (k, v) in &self.env {
                        cmd.env(k, v);
                    }
                    // Without this, tokio::process::Child does NOT kill the OS
                    // process when dropped (it just detaches). If a downstream
                    // server hangs before ever responding, our tokio::time::timeout
                    // cancels this future correctly, but the orphaned child process
                    // would otherwise run forever — kill_on_drop ensures the
                    // timeout actually terminates the hung child, not just our
                    // side of the connection.
                    cmd.kill_on_drop(true);
                },
            ))
            .map_err(|e| GatewayError::Connection(format!("spawn '{}' failed: {e}", self.command)))?;
            let running = tokio::time::timeout(self.timeout, ().serve(transport))
                .await
                .map_err(|_| {
                    GatewayError::Timeout(format!(
                        "connect to '{}' timed out after {:?}",
                        self.command, self.timeout
                    ))
                })?
                .map_err(|e| {
                    GatewayError::Connection(format!("connect to '{}' failed: {e}", self.command))
                })?;
            *guard = Some(running);
        }
        Ok(guard)
    }

    pub async fn discover(&self) -> Result<Vec<ToolEntry>, GatewayError> {
        self.circuit.check(&self.command).await?;
        let result = self.discover_inner().await;
        match &result {
            Ok(_) => self.circuit.record_success().await,
            Err(_) => self.circuit.record_failure().await,
        }
        result
    }

    async fn discover_inner(&self) -> Result<Vec<ToolEntry>, GatewayError> {
        let mut guard = self.ensure_connected().await?;
        let running = guard.as_ref().expect("connected above");
        let tools = match tokio::time::timeout(self.timeout, running.list_all_tools()).await {
            Ok(Ok(tools)) => tools,
            Ok(Err(e)) => {
                // A service-level error here (as opposed to a timeout) can
                // still mean the transport itself died underneath us (broken
                // pipe, child process crashed) rather than the downstream
                // server returning a clean protocol error — rmcp doesn't
                // distinguish the two at this call site. Drop the cached
                // connection so the next discover()/call() respawns rather
                // than repeatedly hitting a dead pipe until something else
                // notices; a healthy connection just pays one extra
                // handshake next time, which is cheap.
                *guard = None;
                return Err(GatewayError::Upstream(e.to_string()));
            }
            Err(_) => {
                // Timing out here means the cached connection produced no
                // response in time — it may be permanently wedged (e.g. the
                // child process hung mid-request). Drop it so the NEXT
                // `discover()`/`call()` respawns a fresh connection instead
                // of `ensure_connected` seeing `guard.is_some()` and quietly
                // reusing the broken one.
                *guard = None;
                return Err(GatewayError::Timeout(format!(
                    "discover on '{}' timed out after {:?}",
                    self.command, self.timeout
                )));
            }
        };
        Ok(tools
            .into_iter()
            .map(|t| ToolEntry {
                name: t.name.to_string(),
                description: t.description.map(|d| d.to_string()).unwrap_or_default(),
                input_schema: Value::Object((*t.input_schema).clone()),
            })
            .collect())
    }

    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, GatewayError> {
        self.circuit.check(&self.command).await?;
        let result = self.call_inner(tool, args).await;
        match &result {
            Ok(_) => self.circuit.record_success().await,
            Err(_) => self.circuit.record_failure().await,
        }
        result
    }

    async fn call_inner(&self, tool: &str, args: Value) -> Result<Value, GatewayError> {
        let mut guard = self.ensure_connected().await?;
        let running = guard.as_ref().expect("connected above");
        let args_map = match args {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => {
                // Local, pre-flight validation — happens before any
                // downstream I/O, so this is the caller's mistake, not the
                // downstream server's. `InvalidArgument`, not `Upstream`,
                // so `gateway_execute` maps it to `invalid_params`.
                return Err(GatewayError::InvalidArgument(format!(
                    "args must be a JSON object, got {other}"
                )))
            }
        };
        let mut params = rmcp::model::CallToolRequestParams::new(tool.to_string());
        params.arguments = args_map;
        let result = match tokio::time::timeout(self.timeout, running.call_tool(params)).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                // See the matching comment in `discover()`: a service-level
                // error here may mean the transport itself died rather than
                // a clean protocol-level failure — drop the cached
                // connection so the next call respawns instead of reusing a
                // possibly-dead pipe.
                *guard = None;
                return Err(GatewayError::Upstream(e.to_string()));
            }
            Err(_) => {
                // See the matching comment in `discover()`: a timed-out
                // cached connection may be permanently wedged, so drop it
                // rather than let the next call reuse it.
                *guard = None;
                return Err(GatewayError::Timeout(format!(
                    "call '{tool}' on '{}' timed out after {:?}",
                    self.command, self.timeout
                )));
            }
        };
        if result.is_error.unwrap_or(false) {
            return Err(GatewayError::Upstream(
                serde_json::to_string(&result.content).unwrap_or_default(),
            ));
        }
        if let Some(structured) = result.structured_content {
            return Ok(structured);
        }
        serde_json::to_value(&result.content)
            .map_err(|e| GatewayError::Upstream(format!("result serialization failed: {e}")))
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
