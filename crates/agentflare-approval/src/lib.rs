//! Fail-closed human-in-the-loop approval gate (#197, child of #131 EPIC).
//!
//! Every acting command is classified into a [`types::CommandClass`]
//! (`Read`/`Write`/`Network`/`Install`/`Destructive`); the configured
//! [`types::AutonomyTier`] decides whether that class runs silently,
//! prompts, or is blocked outright ([`policy::tier_action`]). A prompted
//! call is parked by [`gate::ApprovalGate`], persisted in
//! `agentflare-store` so it survives a restart, and resolved by whichever
//! surface — a channel card or the terminal — calls
//! [`gate::ApprovalGate::decide`] first. A 10-minute TTL fails closed
//! (deny) if nobody answers. Everything persisted or broadcast is redacted
//! first ([`redact`]).

pub mod classifier;
pub mod gate;
pub mod policy;
pub mod redact;
pub mod types;

pub use classifier::{allow_key, classify_command};
pub use gate::ApprovalGate;
pub use policy::tier_action;
pub use types::{
    ApprovalDecision, AutonomyTier, CommandClass, DecidedVia, ExecutionOutcome, GateOutcome,
    Origin, PendingApproval, TierAction,
};
