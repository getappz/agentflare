//! Modules shared by the agentflare root binary, extracted from `src/` so
//! later root-crate splits can reuse them without reaching back into the
//! binary crate (item #655). Internal to this workspace (`publish = false`):
//! the root binary re-exports each module, so existing `crate::X` call sites
//! keep working unchanged.
pub mod dispatch_failure_ceiling;
pub mod errors;
pub mod mise_install;
pub mod paths;
pub mod state;
pub mod store;
