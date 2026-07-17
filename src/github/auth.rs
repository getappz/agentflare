//! Credential resolution for the GitHub client. Order: env → agentflare gateway
//! secret (`github_token`) → `gh auth token`. A missing credential is a hard,
//! actionable error — GitHub allows no anonymous writes.

use crate::github::GitHubError;

pub(crate) const NO_AUTH_MSG: &str = "No GitHub credentials. Set GITHUB_TOKEN, run \
'gh auth login', or store one with 'agentflare gateway secret set github_token'.";

fn nonempty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn env_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .ok()
        .and_then(nonempty)
        .or_else(|| std::env::var("GH_TOKEN").ok().and_then(nonempty))
}

fn secret_token() -> Option<String> {
    let conn = crate::db::open().ok()?;
    crate::gateway_secrets::get_secret(&conn, "github_token")
        .ok()
        .flatten()
        .and_then(nonempty)
}

fn gh_auth_token() -> Option<String> {
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    nonempty(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Resolve a GitHub credential, or `GitHubError::NoAuth` with remediation text.
pub fn resolve_token() -> Result<String, GitHubError> {
    pick_token(env_token(), secret_token(), gh_auth_token())
}

fn pick_token(
    env: Option<String>,
    secret: Option<String>,
    gh: Option<String>,
) -> Result<String, GitHubError> {
    env.or(secret)
        .or(gh)
        .ok_or_else(|| GitHubError::NoAuth(NO_AUTH_MSG.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_prefers_env_then_secret_then_gh() {
        assert_eq!(
            pick_token(Some("e".into()), Some("s".into()), Some("g".into())).unwrap(),
            "e"
        );
        assert_eq!(
            pick_token(None, Some("s".into()), Some("g".into())).unwrap(),
            "s"
        );
        assert_eq!(pick_token(None, None, Some("g".into())).unwrap(), "g");
    }

    #[test]
    fn pick_none_is_noauth() {
        assert!(matches!(
            pick_token(None, None, None).unwrap_err(),
            GitHubError::NoAuth(_)
        ));
    }
}
