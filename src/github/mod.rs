//! GitHub repo management — one place for identity, auth, an HTTP client, and
//! per-resource operations (pull requests, and in later phases issues,
//! releases, actions). Built on the already-present `ureq` + `serde_json`; no
//! new dependency, and sync throughout so the MCP tool stays a plain `fn`.

pub mod actions;
pub mod auth;
pub mod client;
pub mod identity;
pub mod init_auth;
pub mod issues;
pub mod mcp;
pub mod models;
pub mod pulls;
pub mod releases;

#[cfg(test)]
pub(crate) mod test_support;

pub use client::Client;
pub use identity::RepoId;

/// Percent-encode a dynamic value for a URL query string so reserved
/// characters cannot alter query semantics. Unreserved chars and slash
/// (common in branch names, valid in the query component) pass through.
pub(crate) fn encode_query(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || "-._~/".contains(ch) {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for b in ch.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

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
            GitHubError::NoAuth(m) | GitHubError::Forbidden(m) | GitHubError::RateLimited(m) => {
                write!(f, "{m}")
            }
            GitHubError::NotFound => write!(f, "not found"),
            GitHubError::Http { status, body } => write!(f, "GitHub HTTP {status}: {body}"),
            GitHubError::Transport(m) => write!(f, "transport error: {m}"),
            GitHubError::Parse(m) => write!(f, "response parse error: {m}"),
        }
    }
}

impl std::error::Error for GitHubError {}

#[cfg(test)]
mod encode_tests {
    use super::*;

    #[test]
    fn encode_query_neutralizes_injection_and_passes_unreserved() {
        assert_eq!(
            encode_query("feature/a&per_page=1"),
            "feature/a%26per_page%3D1"
        );
        assert_eq!(encode_query("open"), "open");
        assert_eq!(encode_query("a b"), "a%20b");
    }

    #[test]
    fn encode_query_encodes_multibyte_utf8() {
        // Each byte of a non-ASCII char is percent-encoded individually.
        assert_eq!(encode_query("café"), "caf%C3%A9");
    }

    #[test]
    fn github_error_display_matches_each_variant() {
        assert_eq!(
            GitHubError::NoAuth("no creds".into()).to_string(),
            "no creds"
        );
        assert_eq!(
            GitHubError::Forbidden("forbidden".into()).to_string(),
            "forbidden"
        );
        assert_eq!(
            GitHubError::RateLimited("slow down".into()).to_string(),
            "slow down"
        );
        assert_eq!(GitHubError::NotFound.to_string(), "not found");
        assert_eq!(
            GitHubError::Http {
                status: 500,
                body: "boom".into()
            }
            .to_string(),
            "GitHub HTTP 500: boom"
        );
        assert_eq!(
            GitHubError::Transport("reset".into()).to_string(),
            "transport error: reset"
        );
        assert_eq!(
            GitHubError::Parse("bad json".into()).to_string(),
            "response parse error: bad json"
        );
    }
}
