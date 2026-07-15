mod audit;
mod backend;
mod circuit;
mod config;
mod db;
mod error;
mod mcp_http;
mod mcp_stdio;
mod redact;
mod registry;
pub mod registry_search;
mod sanitize;
mod search;
mod truncate;
mod types;

pub use backend::Backend;
pub use config::{ConfigError, GatewayConfig, ServerConfig, parse as parse_config};
pub use error::{GatewayError, suggest};
pub use mcp_http::McpHttpBackend;
pub use mcp_stdio::{DEFAULT_TIMEOUT, McpStdioBackend};
pub use redact::redact_error_for_llm;
pub use registry::Registry;
pub use search::{
    HitSource, InstallHint, MatchMode, REGISTRY_FALLBACK_SCORE, ToolHit, merge_registry_hits,
    search as search_tools,
};
pub use truncate::{DEFAULT_MAX_CHARS, truncate_if_needed};
pub use types::ToolEntry;
