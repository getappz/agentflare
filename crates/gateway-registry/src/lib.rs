mod backend;
mod config;
mod db;
mod error;
mod mcp_stdio;
mod search;
mod truncate;
mod types;

pub use backend::{Backend, HttpApiBackend};
pub use config::{parse as parse_config, GatewayConfig, HttpToolConfig, ServerConfig};
pub use error::{suggest, GatewayError};
pub use mcp_stdio::McpStdioBackend;
pub use search::{search as search_tools, MatchMode, ToolHit};
pub use truncate::{truncate_if_needed, DEFAULT_MAX_CHARS};
pub use types::ToolEntry;
