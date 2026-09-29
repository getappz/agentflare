//! Fail-closed outer-fence guarantees, ported from OpenShell's
//! `OuterFenceGuarantees` (`openshell-isolation-interface/src/contract.rs`).
//!
//! OpenShell's shared isolation contract receives only normalized guarantees
//! from the component that owns the outer network fence: default-deny egress,
//! no unmanaged path, verified revocation, fail-closed controller loss -- plus
//! a digest binding those guarantees to one sandbox generation. The common
//! runtime checks the projection without interpreting backend-native fields.
//!
//! This crate's enforcement owner is the bwrap wrapper: read-only root +
//! explicit writable binds + private `/tmp` + unshared user/pid namespaces.
//! This module projects that into the same four guarantees so callers can
//! check completeness, and adds the fail-closed switch OpenShell gets from its
//! supervisor: when `FLARE_SANDBOX_FAIL_CLOSED` is set, a missing boundary
//! (no bwrap, unresolvable cwd, non-Linux platform) is an error instead of a
//! silent unsandboxed fallback.

use std::fmt;

/// The four normalized guarantees, mirroring OpenShell's
/// `OuterFenceGuarantee` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OuterFenceGuarantee {
    /// No job process can write outside its explicit binds (`--ro-bind / /`
    /// plus enumerated writable binds).
    DefaultDenyWrites,
    /// No unmanaged write path: private tmpfs `/tmp`, no shared `/dev/shm`
    /// bind, `--chdir` into the resolved cwd.
    NoUnmanagedWritePath,
    /// A bad bind (unresolvable cwd, escaped symlink) revokes the whole
    /// sandbox (falls back / errors) instead of mounting a wrong directory.
    RevocationVerified,
    /// Losing the boundary (bwrap missing) fails closed under
    /// `FLARE_SANDBOX_FAIL_CLOSED` instead of running unsandboxed.
    ControllerLossFailsClosed,
}

/// Normalized guarantee set for one sandbox decision, bound to the resolved
/// cwd (the "generation" in OpenShell terms: one resolved cwd per job).
#[derive(Debug, Clone)]
pub struct OuterFenceGuarantees {
    /// Canonicalized cwd this guarantee set was projected for, `None` when
    /// no cwd was supplied.
    pub generation: Option<String>,
    /// Guarantees the bwrap backend establishes. Always complete when a
    /// bwrap invocation is actually built; empty when falling back.
    pub established: Vec<OuterFenceGuarantee>,
}

impl OuterFenceGuarantees {
    /// Project the bwrap backend's guarantees for a resolved cwd.
    ///
    /// `fail_closed` must be the same decision's
    /// [`is_fail_closed`] value: losing the boundary (bwrap missing,
    /// unresolvable cwd) only fails closed when the switch is on, so
    /// `ControllerLossFailsClosed` is established only then. A fail-open
    /// projection is therefore never [`is_complete`](Self::is_complete)
    /// -- by design, not by omission.
    #[must_use]
    pub fn from_bwrap_enforcement(cwd: Option<&str>, fail_closed: bool) -> Self {
        let mut established = vec![
            OuterFenceGuarantee::DefaultDenyWrites,
            OuterFenceGuarantee::NoUnmanagedWritePath,
            OuterFenceGuarantee::RevocationVerified,
        ];
        if fail_closed {
            established.push(OuterFenceGuarantee::ControllerLossFailsClosed);
        }
        Self {
            generation: cwd.map(str::to_string),
            established,
        }
    }

    /// Empty set for the fallback path (no boundary established).
    #[must_use]
    pub fn fallback() -> Self {
        Self {
            generation: None,
            established: Vec::new(),
        }
    }

    /// True only when all four guarantees are present.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.established.len() == 4
            && self
                .established
                .contains(&OuterFenceGuarantee::DefaultDenyWrites)
            && self
                .established
                .contains(&OuterFenceGuarantee::NoUnmanagedWritePath)
            && self
                .established
                .contains(&OuterFenceGuarantee::RevocationVerified)
            && self
                .established
                .contains(&OuterFenceGuarantee::ControllerLossFailsClosed)
    }
}

/// Error returned by [`crate::try_wrap`] when no boundary can be established.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxError {
    /// Machine-readable kind: `unavailable` (no bwrap / non-Linux),
    /// `unresolvable_cwd`, or `invalid_config`.
    pub kind: &'static str,
    /// Human-readable detail (command + reason, no secrets).
    pub message: String,
}

impl SandboxError {
    pub(crate) fn unavailable(command: &str, reason: &str) -> Self {
        Self {
            kind: "unavailable",
            message: format!("sandbox unavailable for {command}: {reason}"),
        }
    }

    /// Only constructed on Linux (see `try_wrap`); allowed dead elsewhere.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn unresolvable_cwd(command: &str) -> Self {
        Self {
            kind: "unresolvable_cwd",
            message: format!("sandbox refused for {command}: cwd does not resolve"),
        }
    }
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for SandboxError {}

/// Fail-closed switch (OpenShell's supervisor-side confirmation, reduced to
/// an env var for a CLI job runner): when `FLARE_SANDBOX_FAIL_CLOSED` is
/// `1`/`true`/`yes` (case-insensitive), [`crate::try_wrap`] returns `Err`
/// instead of falling back to running the job unsandboxed.
#[must_use]
pub fn is_fail_closed() -> bool {
    std::env::var("FLARE_SANDBOX_FAIL_CLOSED")
        .map(|v| parse_fail_closed(&v))
        .unwrap_or(false)
}

/// Pure truthy parser behind [`is_fail_closed`]: unit-tested directly so no
/// test has to mutate the process-global environment (cargo runs tests in
/// one binary on parallel threads).
#[must_use]
pub fn parse_fail_closed(raw: &str) -> bool {
    let v = raw.trim().to_ascii_lowercase();
    v == "1" || v == "true" || v == "yes"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bwrap_projection_is_complete_only_when_fail_closed() {
        let g = OuterFenceGuarantees::from_bwrap_enforcement(Some("/work/tree"), true);
        assert!(g.is_complete());
        assert_eq!(g.generation.as_deref(), Some("/work/tree"));

        // Fail-open keeps three guarantees and is incomplete by design:
        // losing the boundary falls back instead of erroring.
        let g = OuterFenceGuarantees::from_bwrap_enforcement(Some("/work/tree"), false);
        assert!(!g.is_complete());
        assert!(
            g.established
                .contains(&OuterFenceGuarantee::DefaultDenyWrites)
        );
        assert!(
            !g.established
                .contains(&OuterFenceGuarantee::ControllerLossFailsClosed)
        );
    }

    #[test]
    fn fallback_projection_is_incomplete() {
        assert!(!OuterFenceGuarantees::fallback().is_complete());
    }

    #[test]
    fn fail_closed_parser_accepts_truthy_only() {
        for v in ["1", "true", "TRUE", " yes "] {
            assert!(parse_fail_closed(v), "{v} should enable fail-closed");
        }
        for v in ["0", "false", "", "maybe"] {
            assert!(!parse_fail_closed(v), "{v} should not enable fail-closed");
        }
    }
}
