//! Shared types for the approval gate. Kept narrow and dependency-light so
//! `classifier`, `policy`, `redact`, `store`, and `gate` can all import
//! from here without a circular dependency through `lib.rs`.

use serde::{Deserialize, Serialize};

/// Fail-closed command classification (epic #131 design). Ordered by
/// severity — [`CommandClass::rank`] gives the total order used to resolve
/// piped/compound commands ("highest class wins").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandClass {
    /// Provably read-only / observational. Curated allowlist only —
    /// everything not on it is at least `Write`.
    Read,
    /// State-changing. The fail-closed default for anything unrecognized.
    Write,
    /// Reaches the network (curl, wget, ssh, scp, git clone/push, ...).
    Network,
    /// Installs an OS or language package.
    Install,
    /// Catastrophic / irreversible / privilege-escalating.
    Destructive,
}

impl CommandClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Network => "network",
            Self::Install => "install",
            Self::Destructive => "destructive",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "network" => Some(Self::Network),
            "install" => Some(Self::Install),
            "destructive" => Some(Self::Destructive),
            _ => None,
        }
    }
}

/// User-configured autonomy tier (`[autonomy].level`). Determines whether a
/// [`CommandClass`] runs silently, prompts, or is blocked outright — see
/// [`crate::policy::tier_action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyTier {
    /// Read: allow. Everything else: block.
    ReadOnly,
    /// Read: allow. Write/Network/Install/Destructive: prompt. Default tier.
    Supervised,
    /// Read/Write: allow. Network/Install/Destructive: prompt.
    Full,
}

/// What a tier says to do with a given [`CommandClass`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierAction {
    /// Run without prompting.
    Allow,
    /// Park for a human decision.
    Prompt,
    /// Refuse outright. No approval — in-tier or otherwise — can override
    /// this; it never reaches the gate's park/decide path.
    Block,
}

/// Where an intercepted call originated. Only [`Origin::Interactive`] turns
/// are ever parked for a human decision — background/cron turns are
/// pre-authorized by their own policy (the caller decides `Origin` up
/// front, the gate never guesses).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Interactive,
    Background,
    Cron,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Background => "background",
            Self::Cron => "cron",
        }
    }
}

/// The human's decision on a pending approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Run this call once; future calls are gated again.
    ApproveOnce,
    /// Run this call AND persist the command onto the always-allow list.
    ApproveAlways,
    /// Refuse the call.
    Deny,
}

impl ApprovalDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApproveOnce => "approve_once",
            Self::ApproveAlways => "approve_always",
            Self::Deny => "deny",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "approve_once" => Some(Self::ApproveOnce),
            "approve_always" => Some(Self::ApproveAlways),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }

    pub fn is_approve(self) -> bool {
        matches!(self, Self::ApproveOnce | Self::ApproveAlways)
    }
}

/// Which surface committed a decision first (for the first-wins race audit
/// trail): the channel card, the terminal prompt, the TTL timeout, or an
/// always-allow allowlist hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecidedVia {
    Channel,
    Terminal,
    Timeout,
    Allowlist,
}

impl DecidedVia {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Channel => "channel",
            Self::Terminal => "terminal",
            Self::Timeout => "timeout",
            Self::Allowlist => "allowlist",
        }
    }
}

/// Outcome of [`crate::gate::ApprovalGate::intercept`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    Allow,
    Deny { reason: String },
}

/// Terminal status of a tool call the gate previously allowed, recorded via
/// [`crate::gate::ApprovalGate::record_execution`] for the audit trail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    Success,
    Error,
}

impl ExecutionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// A tool call parked awaiting a human decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub request_id: String,
    pub command_class: CommandClass,
    /// Short, redacted human-readable summary safe to persist/broadcast.
    pub action_summary: String,
    /// The command itself, redacted (see [`crate::redact`]).
    pub command_redacted: String,
    pub origin: Origin,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}
