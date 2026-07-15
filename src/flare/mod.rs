//! Flare optimize module — multi-layer compression for AI agents.
//!
//! Layers:
//! - output: LLM-based prose compression (was caveman)
//! - code: Code minimalism rules (was ponytail)
//! - context: Session transcript compaction via FTS5/BM25 (was compact)
//! - runtime: Session hygiene, model routing, batching nudges (was optimize)

pub mod code;
pub mod context;
pub mod output;
pub mod runtime;

// Re-exports for backward compat — old CLI files reference crate::flare::Prompt etc.
#[allow(unused_imports)]
pub use output::{BackupMode, Prompt, RealLlm, compress};

