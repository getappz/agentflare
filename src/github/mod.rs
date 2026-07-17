//! GitHub repo management — one place for identity, auth, an HTTP client, and
//! per-resource operations (pull requests, and in later phases issues,
//! releases, actions). Built on the already-present `ureq` + `serde_json`; no
//! new dependency, and sync throughout so the MCP tool stays a plain `fn`.

pub mod auth;
pub mod client;
pub mod identity;
pub mod issues;
pub mod mcp;
pub mod models;
pub mod pulls;

pub use client::Client;
pub use identity::RepoId;

/// All failure modes of the GitHub module. `Display` never contains the token.
#[derive(Debug)]
pub enum GitHubError {
    NoAuth(String),
    Forbidden(String),
    NotFound,
    RateLimited(String),
    Http { status: u16, body: String },
    Transport(String),
    Parse(String),
}

impl std::fmt::Display for GitHubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitHubError::NoAuth(m) | GitHubError::Forbidden(m) | GitHubError::RateLimited(m) => write!(f, "{m}"),
            GitHubError::NotFound => write!(f, "not found"),
            GitHubError::Http { status, body } => write!(f, "GitHub HTTP {status}: {body}"),
            GitHubError::Transport(m) => write!(f, "transport error: {m}"),
            GitHubError::Parse(m) => write!(f, "response parse error: {m}"),
        }
    }
}

impl std::error::Error for GitHubError {}
