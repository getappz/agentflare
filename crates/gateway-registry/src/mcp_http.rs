//! Real MCP client over the Streamable-HTTP transport, using `rmcp`'s own
//! client transport (`StreamableHttpClientTransport` + the `initialize`
//! handshake via `ServiceExt::serve`) — same shape as `mcp_stdio.rs`'s
//! stdio backend, just a different transport underneath.

use crate::circuit::{CircuitBreaker, CIRCUIT_FAILURE_THRESHOLD, CIRCUIT_RECOVERY_TIMEOUT};
use crate::error::GatewayError;
use crate::types::ToolEntry;
use http::{HeaderName, HeaderValue};
use rmcp::{
    service::RunningService,
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
    },
    RoleClient, ServiceExt,
};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

use crate::mcp_stdio::DEFAULT_TIMEOUT;

pub struct McpHttpBackend {
    running: tokio::sync::Mutex<Option<RunningService<RoleClient, ()>>>,
    circuit: CircuitBreaker,
    url: String,
    /// Resolved (header name, secret value) pair, or `None` if this server
    /// has no `auth_ref` configured. Resolution (secrets-store lookup)
    /// happens in `registry.rs::build_backends`, same as stdio's `env`
    /// injection — this struct just holds the already-resolved value.
    auth_header: Option<(String, String)>,
    timeout: Duration,
}

impl McpHttpBackend {
    pub fn new(url: String, auth_header: Option<(String, String)>) -> Self {
        Self::with_timeout_and_circuit_recovery(url, auth_header, DEFAULT_TIMEOUT, CIRCUIT_RECOVERY_TIMEOUT)
    }

    /// Same as [`Self::new`] but with an explicit timeout and circuit
    /// recovery window — exists so tests can exercise the timeout/recovery
    /// paths without waiting out the real defaults; production callers
    /// should use `new`.
    pub fn with_timeout_and_circuit_recovery(
        url: String,
        auth_header: Option<(String, String)>,
        timeout: Duration,
        circuit_recovery: Duration,
    ) -> Self {
        Self {
            running: tokio::sync::Mutex::new(None),
            circuit: CircuitBreaker::new(CIRCUIT_FAILURE_THRESHOLD, circuit_recovery),
            url,
            auth_header,
            timeout,
        }
    }

    async fn ensure_connected(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<RunningService<RoleClient, ()>>>, GatewayError> {
        let mut guard = self.running.lock().await;
        if guard.is_none() {
            let mut cfg = StreamableHttpClientTransportConfig::with_uri(self.url.clone());
            if let Some((name, value)) = &self.auth_header {
                let mut headers = HashMap::new();
                let header_name = HeaderName::from_bytes(name.as_bytes())
                    .map_err(|e| GatewayError::Connection(format!("invalid header name '{name}': {e}")))?;
                let header_value = HeaderValue::from_str(value)
                    .map_err(|e| GatewayError::Connection(format!("invalid header value for '{name}': {e}")))?;
                headers.insert(header_name, header_value);
                cfg = cfg.custom_headers(headers);
            }
            let transport = StreamableHttpClientTransport::from_config(cfg);
            let running = tokio::time::timeout(self.timeout, ().serve(transport))
                .await
                .map_err(|_| {
                    GatewayError::Timeout(format!(
                        "connect to '{}' timed out after {:?}",
                        self.url, self.timeout
                    ))
                })?
                .map_err(|e| {
                    GatewayError::Connection(format!("connect to '{}' failed: {e}", self.url))
                })?;
            *guard = Some(running);
        }
        Ok(guard)
    }

    pub async fn discover(&self) -> Result<Vec<ToolEntry>, GatewayError> {
        self.circuit.check(&self.url).await?;
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
                *guard = None;
                return Err(GatewayError::Upstream(e.to_string()));
            }
            Err(_) => {
                *guard = None;
                return Err(GatewayError::Timeout(format!(
                    "discover on '{}' timed out after {:?}",
                    self.url, self.timeout
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
        self.circuit.check(&self.url).await?;
        let result = self.call_inner(tool, args).await;
        match &result {
            Ok(_) => self.circuit.record_success().await,
            Err(_) => self.circuit.record_failure().await,
        }
        result
    }

    async fn call_inner(&self, tool: &str, args: Value) -> Result<Value, GatewayError> {
        // Validated before `ensure_connected()`, not after: this is a local,
        // pre-flight check with no downstream I/O involved, so a malformed
        // call fails instantly instead of first paying for (and holding the
        // process-wide gateway lock across) a connect attempt that was
        // never going to succeed regardless of the args. Mirrors
        // `mcp_stdio.rs::call_inner`'s identical ordering.
        let args_map = match args {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => {
                return Err(GatewayError::InvalidArgument(format!(
                    "args must be a JSON object, got {other}"
                )))
            }
        };
        let mut guard = self.ensure_connected().await?;
        let running = guard.as_ref().expect("connected above");
        let mut params = rmcp::model::CallToolRequestParams::new(tool.to_string());
        params.arguments = args_map;
        let result = match tokio::time::timeout(self.timeout, running.call_tool(params)).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                *guard = None;
                return Err(GatewayError::Upstream(e.to_string()));
            }
            Err(_) => {
                *guard = None;
                return Err(GatewayError::Timeout(format!(
                    "call '{tool}' on '{}' timed out after {:?}",
                    self.url, self.timeout
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
