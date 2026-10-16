//! # flare-consensus
//!
//! Multi-model debate/consensus engine: several LLM participants argue a
//! question across rounds — initial analysis, counterarguments, evidence
//! assessment, synthesis — with confidence scoring, pairwise disagreement
//! detection, early stopping on convergence, and an optional non-voting
//! judge that synthesises a majority/minority verdict.
//!
//! Design lineage: ported from `ai-consensus-core` 0.10
//! (entropyvortex/ai-consensus-core, MIT) — the Consensus Validation
//! Protocol (CVP) engine that also powers Roundtable. Provider calling and
//! the tool-call loop were dropped; round scheduling, prompt shapes,
//! confidence parsing, stats, and judge synthesis are a faithful port.
//! `ModelCaller` is this crate's single extension point — `flare-desktop`
//! wires it to `flare-proxy`, which already speaks to every provider
//! (Anthropic, OpenAI, Gemini, xAI, Perplexity, ...) this engine needs.
//!
//! Event delivery (`events::EventBus`/`EventSubscriber`) follows the same
//! shape as `flare-workflow`'s event bus, for consistency across the
//! workspace's engine crates.

pub mod engine;
pub mod events;
pub mod parser;
pub mod personas;
pub mod prompts;
pub mod stats;
pub mod types;

pub use engine::{ConsensusDefaults, ConsensusEngine, ConsensusError, MAX_ROUNDS_CAP};
pub use events::{ConsensusEvent, EventBus, EventSubscriber};
pub use personas::judge_persona;
pub use types::*;
