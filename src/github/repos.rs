//! Repository-level GitHub API calls (currently just default-branch lookup).

use crate::github::{Client, GitHubError, RepoId};

/// Fetches `repo`'s default branch via the GitHub API. Used when an explicit
/// `repo` override is given, since there's no local checkout to read it from.
pub fn get_default_branch(client: &Client, repo: &RepoId) -> Result<String, GitHubError> {
    let path = format!("/repos/{}/{}", repo.owner, repo.repo);
    let json = client.request("GET", &path, None)?;
    json.get("default_branch")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| GitHubError::Parse("missing default_branch".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::test_support::{MockResponse, MockServer};

    fn repo() -> RepoId {
        RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    #[test]
    fn get_default_branch_reads_the_field() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"default_branch":"main"}"#,
        )]);
        let client = server.client(None);
        assert_eq!(get_default_branch(&client, &repo()).unwrap(), "main");
        assert_eq!(server.requests()[0].path, "/repos/o/r");
    }

    #[test]
    fn get_default_branch_errors_when_field_missing() {
        let server = MockServer::start(vec![MockResponse::json(200, "{}")]);
        let client = server.client(None);
        assert!(get_default_branch(&client, &repo()).is_err());
        let _ = server.requests();
    }
}
