//! Repository-level GitHub API calls: default-branch lookup, the repo
//! settings branch hygiene needs, and post-merge head-branch deletion.

use crate::github::{Client, GitHubError, RepoId};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The slice of `GET /repos/{owner}/{repo}` post-merge cleanup consults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSettings {
    pub default_branch: String,
    /// GitHub's own "Automatically delete head branches" setting -- when on,
    /// GitHub already deleted the branch at merge time.
    pub delete_branch_on_merge: bool,
}

/// `repo`'s settings, fetched once per process per host+repo: they change
/// about never, and the review sweep would otherwise re-read them for every
/// merged PR. Keyed by the client's host as well so concurrently running
/// tests against different mock servers never see each other's answers.
pub fn settings(client: &Client, repo: &RepoId) -> Result<RepoSettings, GitHubError> {
    static CACHE: OnceLock<Mutex<HashMap<String, RepoSettings>>> = OnceLock::new();
    let key = format!("{} {}/{}", client.host_key(), repo.owner, repo.repo);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned()
    {
        return Ok(hit);
    }
    let json = client.request("GET", &format!("/repos/{}/{}", repo.owner, repo.repo), None)?;
    let fetched = RepoSettings {
        default_branch: json["default_branch"]
            .as_str()
            .ok_or_else(|| GitHubError::Parse("missing default_branch".to_string()))?
            .to_string(),
        delete_branch_on_merge: json["delete_branch_on_merge"].as_bool().unwrap_or(false),
    };
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, fetched.clone());
    Ok(fetched)
}

/// What [`delete_merged_pr_branch`] did with a merged PR's head branch.
#[derive(Debug, PartialEq, Eq)]
pub enum BranchCleanup {
    Deleted,
    /// The PR isn't merged (yet) -- nothing is deleted.
    NotMerged,
    /// The repo has `delete_branch_on_merge` on; GitHub handles it.
    GitHubAutoDeletes,
    /// The head is the default branch or a protected one -- never ours to
    /// delete.
    Protected,
    /// The head lives in a fork (or a deleted one): deleting a same-named
    /// ref here would hit the wrong branch.
    Foreign,
    /// Somebody (or GitHub) already deleted it.
    AlreadyGone,
    /// The branch tip is no longer the merged PR's head commit (something
    /// was pushed after the merge) -- deleting it would lose that commit.
    Advanced,
}

/// Deletes merged PR `number`'s head branch from `repo`, the remote half of
/// branch hygiene after an auto-merge -- local branches/worktrees are left to
/// worktree cleanup. Re-reads the PR rather than trusting the caller's
/// snapshot, and refuses anything that is not a merged, same-repo,
/// unprotected, non-default branch still pointing at the PR's merged head.
pub fn delete_merged_pr_branch(
    client: &Client,
    repo: &RepoId,
    number: u64,
) -> Result<BranchCleanup, GitHubError> {
    let pr = crate::github::pulls::get(client, repo, number)?;
    if pr.merged_at.is_none() {
        return Ok(BranchCleanup::NotMerged);
    }
    let own_repo = format!("{}/{}", repo.owner, repo.repo);
    let Some(head) = pr.head.filter(|h| {
        h.repo
            .as_ref()
            .is_some_and(|r| r.full_name.eq_ignore_ascii_case(&own_repo))
    }) else {
        return Ok(BranchCleanup::Foreign);
    };
    let settings = settings(client, repo)?;
    if settings.delete_branch_on_merge {
        return Ok(BranchCleanup::GitHubAutoDeletes);
    }
    if head.git_ref == settings.default_branch {
        return Ok(BranchCleanup::Protected);
    }
    let branch = crate::github::encode_query(&head.git_ref);
    let branch_path = format!("/repos/{}/{}/branches/{branch}", repo.owner, repo.repo);
    match client.request("GET", &branch_path, None) {
        Ok(json) if json["protected"].as_bool().unwrap_or(false) => {
            return Ok(BranchCleanup::Protected);
        }
        // Fail closed: a tip we can't read counts as advanced too.
        Ok(json) if json["commit"]["sha"].as_str() != Some(head.sha.as_str()) => {
            return Ok(BranchCleanup::Advanced);
        }
        Ok(_) => {}
        Err(GitHubError::NotFound) => return Ok(BranchCleanup::AlreadyGone),
        Err(e) => return Err(e),
    }
    let ref_path = format!(
        "/repos/{}/{}/git/refs/heads/{branch}",
        repo.owner, repo.repo
    );
    match client.request("DELETE", &ref_path, None) {
        Ok(_) => Ok(BranchCleanup::Deleted),
        // 422 "Reference does not exist": deleted between the two calls.
        Err(GitHubError::NotFound) | Err(GitHubError::Http { status: 422, .. }) => {
            Ok(BranchCleanup::AlreadyGone)
        }
        Err(e) => Err(e),
    }
}

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

    fn merged_pr(head_repo: &str, head_ref: &str) -> String {
        serde_json::json!({
            "number": 5, "html_url": "u", "state": "closed", "title": "t",
            "merged_at": "2026-09-20T00:00:00Z",
            "head": {"ref": head_ref, "sha": "abc", "repo": {"full_name": head_repo}}
        })
        .to_string()
    }

    // Each test uses its own mock server, so the host-keyed settings cache
    // never carries one test's repo settings into another.
    #[test]
    fn delete_merged_pr_branch_deletes_an_unprotected_same_repo_head() {
        let server = MockServer::start(vec![
            MockResponse::json(200, &merged_pr("o/r", "task/5")),
            MockResponse::json(
                200,
                r#"{"default_branch":"main","delete_branch_on_merge":false}"#,
            ),
            MockResponse::json(
                200,
                r#"{"name":"task/5","protected":false,"commit":{"sha":"abc"}}"#,
            ),
            MockResponse::json(204, ""),
        ]);
        let client = server.client(Some("tok"));
        assert_eq!(
            delete_merged_pr_branch(&client, &repo(), 5).unwrap(),
            BranchCleanup::Deleted
        );
        let reqs = server.requests();
        assert_eq!(reqs[2].path, "/repos/o/r/branches/task/5");
        assert_eq!(reqs[3].method, "DELETE");
        assert_eq!(reqs[3].path, "/repos/o/r/git/refs/heads/task/5");
    }

    #[test]
    fn delete_merged_pr_branch_keeps_a_branch_advanced_past_the_merged_head() {
        let server = MockServer::start(vec![
            MockResponse::json(200, &merged_pr("o/r", "task/5")),
            MockResponse::json(
                200,
                r#"{"default_branch":"main","delete_branch_on_merge":false}"#,
            ),
            MockResponse::json(
                200,
                r#"{"name":"task/5","protected":false,"commit":{"sha":"pushed-after-merge"}}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        assert_eq!(
            delete_merged_pr_branch(&client, &repo(), 5).unwrap(),
            BranchCleanup::Advanced
        );
        let reqs = server.requests();
        assert_eq!(reqs.len(), 3);
        assert!(
            reqs.iter().all(|r| r.method == "GET"),
            "no DELETE may be sent"
        );
    }

    #[test]
    fn delete_merged_pr_branch_leaves_it_to_github_when_auto_delete_is_on() {
        let server = MockServer::start(vec![
            MockResponse::json(200, &merged_pr("o/r", "task/5")),
            MockResponse::json(
                200,
                r#"{"default_branch":"main","delete_branch_on_merge":true}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        assert_eq!(
            delete_merged_pr_branch(&client, &repo(), 5).unwrap(),
            BranchCleanup::GitHubAutoDeletes
        );
        assert_eq!(server.requests().len(), 2, "no DELETE may be sent");
    }

    #[test]
    fn delete_merged_pr_branch_refuses_protected_default_and_fork_heads() {
        let server = MockServer::start(vec![
            MockResponse::json(200, &merged_pr("o/r", "release")),
            MockResponse::json(
                200,
                r#"{"default_branch":"main","delete_branch_on_merge":false}"#,
            ),
            MockResponse::json(200, r#"{"name":"release","protected":true}"#),
            MockResponse::json(200, &merged_pr("o/r", "main")),
            MockResponse::json(200, &merged_pr("someone/fork", "task/5")),
        ]);
        let client = server.client(Some("tok"));
        assert_eq!(
            delete_merged_pr_branch(&client, &repo(), 5).unwrap(),
            BranchCleanup::Protected
        );
        // Settings are cached now, so the default-branch check needs no refetch.
        assert_eq!(
            delete_merged_pr_branch(&client, &repo(), 5).unwrap(),
            BranchCleanup::Protected
        );
        assert_eq!(
            delete_merged_pr_branch(&client, &repo(), 5).unwrap(),
            BranchCleanup::Foreign
        );
        let reqs = server.requests();
        assert_eq!(reqs.len(), 5);
        assert!(reqs.iter().all(|r| r.method == "GET"));
    }

    #[test]
    fn delete_merged_pr_branch_skips_an_unmerged_pr() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"number":5,"html_url":"u","state":"open","title":"t"}"#,
        )]);
        let client = server.client(Some("tok"));
        assert_eq!(
            delete_merged_pr_branch(&client, &repo(), 5).unwrap(),
            BranchCleanup::NotMerged
        );
    }
}
