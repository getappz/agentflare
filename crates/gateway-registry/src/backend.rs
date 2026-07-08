//! Backend dispatch. `kind` in config.rs is the seam for future backend
//! types — `Registry::execute` (Task 9) matches on whichever `Backend`
//! variant a server resolved to, so adding a working second kind later is
//! additive (new variant + match arm), not a rewrite of the two MCP tools'
//! external contract.

use crate::config::HttpToolConfig;
use crate::error::GatewayError;
use crate::mcp_stdio::McpStdioBackend;
use crate::types::ToolEntry;
use serde_json::Value;

/// Unimplemented in v1 — see the design spec's "Non-goals" section. No
/// untested request-building code against a hypothetical API; this exists
/// only so `kind = "http_api"` parses and fails clearly instead of silently
/// doing nothing.
pub struct HttpApiBackend {
    pub base_url: String,
    pub tools: Vec<HttpToolConfig>,
}

impl HttpApiBackend {
    pub async fn discover(&self) -> Result<Vec<ToolEntry>, GatewayError> {
        Err(GatewayError::NotImplemented("http_api".into()))
    }

    pub async fn call(&self, _tool: &str, _args: Value) -> Result<Value, GatewayError> {
        Err(GatewayError::NotImplemented("http_api".into()))
    }
}

pub enum Backend {
    McpStdio(McpStdioBackend),
    HttpApi(HttpApiBackend),
}

impl Backend {
    pub async fn discover(&self) -> Result<Vec<ToolEntry>, GatewayError> {
        match self {
            Backend::McpStdio(b) => b.discover().await,
            Backend::HttpApi(b) => b.discover().await,
        }
    }

    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, GatewayError> {
        match self {
            Backend::McpStdio(b) => b.call(tool, args).await,
            Backend::HttpApi(b) => b.call(tool, args).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn http_api_discover_is_not_implemented() {
        let backend = Backend::HttpApi(HttpApiBackend { base_url: "https://x".into(), tools: vec![] });
        let err = backend.discover().await.unwrap_err();
        assert!(matches!(err, GatewayError::NotImplemented(k) if k == "http_api"));
    }

    #[tokio::test]
    async fn http_api_call_is_not_implemented() {
        let backend = Backend::HttpApi(HttpApiBackend { base_url: "https://x".into(), tools: vec![] });
        let err = backend.call("get_forecast", serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, GatewayError::NotImplemented(k) if k == "http_api"));
    }
}
