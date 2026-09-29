//! Isolation-backend contract, ported from OpenShell's
//! `IsolationBackend` (`openshell-isolation-interface/src/contract.rs`).
//!
//! OpenShell drives every compute placement (Docker, Podman, Kubernetes, VM,
//! extension drivers) through one object-safe contract: a registry maps a
//! backend name to its implementation, and the boundary advances through
//! boxed lifecycle states without the supervisor branching on placement.
//!
//! This crate has one backend today (bwrap on Linux) plus the identity
//! passthrough everywhere else. The trait keeps that selection explicit and
//! gives future backends (direct Landlock, Docker, macOS Seatbelt) a seam
//! to plug into without rewriting [`crate::wrap`] callers:
//!
//! ```text
//! resolve backend -> check guarantees -> build argv or fall back
//! ```

use std::path::Path;

use crate::SandboxConfig;

/// One sandbox boundary implementation.
pub trait IsolationBackend {
    /// Stable registered name, e.g. `bwrap`, `passthrough`.
    fn backend_name(&self) -> &'static str;

    /// Whether this backend can enforce a boundary on this machine
    /// (platform + binary presence, no side effects).
    fn is_available(&self) -> bool;

    /// Build the sandboxed argv, or `None` when this backend cannot sandbox
    /// this invocation (missing binary, unresolvable cwd).
    fn wrap(
        &self,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        git_writable: bool,
        config: &SandboxConfig,
        diagnostic_out: Option<&Path>,
    ) -> Option<(String, Vec<String>)>;
}

/// Bubblewrap backend: enforced boundary on Linux/WSL2 only.
#[derive(Debug, Clone, Copy, Default)]
pub struct BwrapBackend;

impl IsolationBackend for BwrapBackend {
    fn backend_name(&self) -> &'static str {
        "bwrap"
    }

    #[cfg(target_os = "linux")]
    fn is_available(&self) -> bool {
        crate::bwrap::probe_available()
    }

    #[cfg(not(target_os = "linux"))]
    fn is_available(&self) -> bool {
        false
    }

    fn wrap(
        &self,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
        git_writable: bool,
        config: &SandboxConfig,
        diagnostic_out: Option<&Path>,
    ) -> Option<(String, Vec<String>)> {
        #[cfg(target_os = "linux")]
        {
            crate::bwrap::wrap(command, args, cwd, git_writable, config, diagnostic_out)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (command, args, cwd, git_writable, config, diagnostic_out);
            None
        }
    }
}

/// Identity backend: never enforces, always falls back. Exists so the
/// registry (and `try_wrap`'s fail-closed branch) can name the fallback
/// instead of scattering `None` handling across callers.
#[derive(Debug, Clone, Copy, Default)]
pub struct PassthroughBackend;

impl IsolationBackend for PassthroughBackend {
    fn backend_name(&self) -> &'static str {
        "passthrough"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn wrap(
        &self,
        _command: &str,
        _args: &[String],
        _cwd: Option<&Path>,
        _git_writable: bool,
        _config: &SandboxConfig,
        _diagnostic_out: Option<&Path>,
    ) -> Option<(String, Vec<String>)> {
        None
    }
}

/// Registry mapping backend names to implementations. The only lookup by
/// name; selection never falls back to another backend implicitly -- the
/// caller picks the first *available* backend in preference order.
#[derive(Default)]
pub struct BackendRegistry {
    backends: Vec<&'static str>,
}

impl BackendRegistry {
    /// Registry with the compiled-in backends in preference order.
    #[must_use]
    pub fn with_builtin_backends() -> Self {
        Self {
            backends: vec![
                BwrapBackend.backend_name(),
                PassthroughBackend.backend_name(),
            ],
        }
    }

    /// Select the first available backend for this machine.
    #[must_use]
    pub fn select(&self) -> &'static str {
        let bwrap = BwrapBackend;
        if self.backends.contains(&bwrap.backend_name()) && bwrap.is_available() {
            return bwrap.backend_name();
        }
        PassthroughBackend.backend_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lists_bwrap_first() {
        let registry = BackendRegistry::with_builtin_backends();
        assert_eq!(registry.backends, vec!["bwrap", "passthrough"]);
    }

    #[test]
    fn select_names_a_registered_backend() {
        let name = BackendRegistry::with_builtin_backends().select();
        assert!(name == "bwrap" || name == "passthrough");
    }

    #[test]
    fn passthrough_never_wraps() {
        let backend = PassthroughBackend;
        assert!(backend.is_available());
        assert!(
            backend
                .wrap("true", &[], None, false, &SandboxConfig::default(), None)
                .is_none()
        );
    }
}
