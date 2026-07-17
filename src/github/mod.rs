//! GitHub repo management — one place for identity, auth, an HTTP client, and
//! per-resource operations (pull requests, and in later phases issues,
//! releases, actions). Built on the already-present `ureq` + `serde_json`; no
//! new dependency, and sync throughout so the MCP tool stays a plain `fn`.

pub mod identity;

pub use identity::{RepoId, normalize_repo};
