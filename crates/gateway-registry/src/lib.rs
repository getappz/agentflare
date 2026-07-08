mod backend;
mod config;
mod db;
mod error;
mod mcp_stdio;
mod registry;
mod search;
mod truncate;
mod types;

pub use backend::{Backend, HttpApiBackend};
pub use config::{parse as parse_config, ConfigError, GatewayConfig, HttpToolConfig, ServerConfig};
pub use error::{suggest, GatewayError};
pub use mcp_stdio::{McpStdioBackend, DEFAULT_TIMEOUT};
pub use registry::Registry;
pub use search::{search as search_tools, MatchMode, ToolHit};
pub use truncate::{truncate_if_needed, DEFAULT_MAX_CHARS};
pub use types::ToolEntry;
