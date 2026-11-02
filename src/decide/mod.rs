//! `decide`: agentflare's typed-decision layer. Ask a small, fast model a few
//! typed questions (choice / score / noul) about some text and get calibrated
//! answers back, instead of keyword heuristics or a full LLM agent turn. The
//! backend is TypeSafe's Jev (System One), reached via OpenRouter or Cloudflare;
//! Jev never generates text, so writing stays with the LLM.
//!
//! Contract for every caller:
//! - opt-in (`AGENTFLARE_JEV=1`), so nothing leaves the machine by default;
//! - credentials are looked up in the process env, then the agentflare vault
//!   (`agentflare vault set OPENROUTER_API_KEY`), then `~/.env`;
//! - fail open: any `DecideError` means "use the existing code path";
//! - `state` leaves the machine, so send only the fields the question needs;
//! - never use it for security/guard decisions (Jev doesn't treat state as hostile).
#![allow(dead_code, unused_imports)] // consumers land in items #702-#705

pub mod breaker;
mod budget;
pub mod capture;
mod client;
#[cfg(test)]
mod ledger_tests;
mod model_identity;
#[cfg(test)]
mod model_identity_tests;
pub mod model_routing;
mod provider;
mod recovery;
pub mod report;
pub mod shadow;
mod storage;
pub mod tracking;
mod types;
mod writer;

pub use budget::{Budget, Limits};
pub use client::{Batch, Outcome, Source, ask, ask_with, ask_within, credential_sources};
pub use provider::{Config, DecideError};
pub use recovery::entrypoint as recovery_entrypoint;
pub use storage::exit;
pub use storage::flush;
pub use storage::shutdown;
pub use types::{Answer, Question, Response, Usage};
