//! flare-insights — unified observability for AI coding agent sessions.
//!
//! Local ingestion, replay, search, analytics, and handoff for coding sessions.
//!
//! Layers:
//! - ingest: adapters for Claude/Codex/OpenCode/Cursor + watcher (Gemini/Copilot not yet)
//! - store: SQLite FTS5 + trigram index (local-first, 127.0.0.1 only)
//! - model: unified Session/Turn/ToolCall/Subagent schema
//! - search: FTS5 + trigram + hybrid (flare-search-kit ready)
//! - analytics: token/cost/cache, heatmaps, tool freq
//! - replay: turn → generation → tool span timeline
//! - handoff: convert claude↔codex + handoff doc generation
//! - export: JSON/JSONL/HTML/DeepEval/OpenAI Evals
//! - api: HTTP API and polling dashboard

pub mod analytics;
pub mod capability;
pub mod config;
pub mod discover;
pub mod evidence;
pub mod export;
pub mod forensics;
pub mod handoff;
pub mod ingest;
pub mod migrations;
pub mod model;
pub mod proposals;
pub mod recall;
pub mod replay;
pub mod search;
pub mod store;
pub mod store_query;

pub mod api;

pub use config::{InsightsConfig, PricingTable};
pub use model::*;
pub use store::InsightsStore;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
