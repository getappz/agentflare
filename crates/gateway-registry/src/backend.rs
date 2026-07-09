//! Backend dispatch. `kind` in config.rs is the seam for future backend
//! types — `Registry::execute` matches on whichever `Backend` variant a
//! server resolved to, so adding a new kind is additive (new variant + match
//! arm), not a rewrite of the two MCP tools' external contract.

use crate::error::GatewayError;
use crate::mcp_http::McpHttpBackend;
use crate::mcp_stdio::McpStdioBackend;
use crate::types::ToolEntry;
use serde_json::Value;

pub enum Backend {
    McpStdio(McpStdioBackend),
    McpHttp(McpHttpBackend),
}

impl Backend {
    pub async fn discover(&self) -> Result<Vec<ToolEntry>, GatewayError> {
        match self {
            Backend::McpStdio(b) => b.discover().await,
            Backend::McpHttp(b) => b.discover().await,
        }
    }

    pub async fn call(&self, tool: &str, args: Value) -> Result<Value, GatewayError> {
        match self {
            Backend::McpStdio(b) => b.call(tool, args).await,
            Backend::McpHttp(b) => b.call(tool, args).await,
        }
    }
}
