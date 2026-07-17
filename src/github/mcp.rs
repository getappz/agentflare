//! Bridges the GitHub operations to the `flare_git` MCP tool. Kept thin: the
//! `mcp_server` tool method validates fields and calls into here; this module
//! classifies `GitHubError` so the server can map it to the right `ErrorData`.

use crate::github::GitHubError;

/// `true` = a client mistake / auth problem (invalid_params); `false` =
/// internal/transport failure (internal_error).
pub fn is_client_error(err: &GitHubError) -> bool {
    matches!(
        err,
        GitHubError::NoAuth(_) | GitHubError::Forbidden(_) | GitHubError::NotFound
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_and_notfound_are_client_errors_transport_is_not() {
        assert!(is_client_error(&GitHubError::NoAuth("x".into())));
        assert!(is_client_error(&GitHubError::NotFound));
        assert!(!is_client_error(&GitHubError::RateLimited("x".into())));
        assert!(!is_client_error(&GitHubError::Transport("x".into())));
        assert!(!is_client_error(&GitHubError::Parse("x".into())));
    }
}
