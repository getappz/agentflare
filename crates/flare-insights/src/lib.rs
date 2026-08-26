//! flare-insights — unified observability for AI coding agent sessions.
//!
//! Superset of agent-trail / agentsview / agent-lens / codedash /
//! vscode-monitor / opensync / claude-monitor / cogpit / langfuse.
//!
//! Layers:
//! - ingest: adapters for Claude/Codex/OpenCode/Cursor/Gemini/Copilot + watcher
//! - store: SQLite FTS5 + trigram index (local-first, 127.0.0.1 only)
//! - model: unified Session/Turn/ToolCall/Subagent schema
//! - search: FTS5 + trigram + hybrid (flare-search-kit ready)
//! - analytics: token/cost/cache, heatmaps, tool freq
//! - replay: turn → generation → tool span timeline
//! - handoff: convert claude↔codex + handoff doc generation
//! - export: DeepEval/JSONL/HTML/tar + Langfuse/Helicone sink
//! - api: REST + WebSocket push

pub mod analytics;
pub mod config;
pub mod export;
pub mod handoff;
pub mod ingest;
pub mod model;
pub mod replay;
pub mod search;
pub mod store;

#[cfg(feature = "api")]
pub mod api;

pub use config::{InsightsConfig, PricingTable};
pub use model::*;
pub use store::InsightsStore;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
