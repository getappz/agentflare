//! flared — always-on supervisor for AI-agent workload hygiene.
//!
//! Lifecycle: audit -> classify -> protect -> lease -> clean.
//! Safety invariant: flared only ever auto-kills processes it holds a valid
//! lease for, and only after re-verifying the process identity fingerprint
//! (exe name + start time) so a reused PID is never killed by mistake.

pub mod actions;
pub mod artifacts;
pub mod config;
pub mod daemon;
pub mod events;
pub mod http;
pub mod janitor;
pub mod leases;
pub mod model;
pub mod policy;
pub mod scanner;
pub mod service;
