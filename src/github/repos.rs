//! Repository-level GitHub API calls: default-branch lookup, the repo
//! settings branch hygiene needs, and post-merge head-branch deletion.

use crate::github::{Client, GitHubError, RepoId};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// The slice of `GET /repos/{owner}/{repo}` the merge path and post-merge
/// cleanup consult.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSettings {
    pub default_branch: String,
    /// GitHub's own "Automatically delete head branches" setting -- when on,
    /// GitHub already deleted the branch at merge time.
    pub delete_branch_on_merge: bool,
    /// "Allow auto-merge" in the repo's settings: whether
    /// `enablePullRequestAutoMerge` can be used at all.
    pub allow_auto_merge: bool,
    pub allow_squash_merge: bool,
    pub allow_merge_commit: bool,
    pub allow_rebase_merge: bool,
}

/// A PR merge method in both spellings GitHub uses: REST's `merge_method`
/// (`squash`) and GraphQL's `PullRequestMergeMethod` enum (`SQUASH`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    pub fn rest(self) -> &'static str {
        match self {
            MergeMethod::Squash => "squash",
            MergeMethod::Merge => "merge",
            MergeMethod::Rebase => "rebase",
        }
    }

    pub fn graphql(self) -> &'static str {
        match self {
            MergeMethod::Squash => "SQUASH",
            MergeMethod::Merge => "MERGE",
            MergeMethod::Rebase => "REBASE",
        }
    }
}

impl RepoSettings {
    /// What GitHub falls back to when it can't read a repo's settings: every
    /// merge method allowed (GitHub's defaults), auto-merge off. Used when
    /// the settings fetch itself fails, so a merge can still be attempted.
    pub fn unknown(default_branch: &str) -> RepoSettings {
        RepoSettings {
            default_branch: default_branch.to_string(),
            delete_branch_on_merge: false,
            allow_auto_merge: false,
            allow_squash_merge: true,
            allow_merge_commit: true,
            allow_rebase_merge: true,
        }
    }

    /// The merge method the supervisor uses for this repo: squash when the
    /// repo allows it (one commit per item, matching agentflare's own
    /// convention), else a merge commit, else rebase. A repo that allows
    /// none -- possible when the fetched flags are all false -- still gets
    /// squash, so the attempt is made and GitHub says why it can't.
    pub fn merge_method(&self) -> MergeMethod {
        if self.allow_squash_merge || (!self.allow_merge_commit && !self.allow_rebase_merge) {
            MergeMethod::Squash
        } else if self.allow_merge_commit {
            MergeMethod::Merge
        } else {
            MergeMethod::Rebase
        }
    }
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
    // The `allow_*` flags default to GitHub's own defaults when the response
    // omits them (a token without admin scope still sees them; a truncated
    // test fixture may not).
    let flag = |name: &str, default: bool| json[name].as_bool().unwrap_or(default);
    let fetched = RepoSettings {
        default_branch: json["default_branch"]
            .as_str()
            .ok_or_else(|| GitHubError::Parse("missing default_branch".to_string()))?
            .to_string(),
        delete_branch_on_merge: flag("delete_branch_on_merge", false),
        allow_auto_merge: flag("allow_auto_merge", false),
        allow_squash_merge: flag("allow_squash_merge", true),
        allow_merge_commit: flag("allow_merge_commit", true),
        allow_rebase_merge: flag("allow_rebase_merge", true),
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
}

/// Deletes merged PR `number`'s head branch from `repo`, the remote half of
/// branch hygiene after an auto-merge -- local branches/worktrees are left to
/// worktree cleanup. Re-reads the PR rather than trusting the caller's
/// snapshot, and refuses anything that is not a merged, same-repo,
/// unprotected, non-default branch.
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
    fn settings_reads_merge_flags_and_defaults_the_missing_ones() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"default_branch":"main","allow_auto_merge":true,"allow_squash_merge":false}"#,
        )]);
        let client = server.client(None);
        let s = settings(&client, &repo()).unwrap();
        assert!(s.allow_auto_merge);
        assert!(!s.allow_squash_merge);
        assert!(s.allow_merge_commit, "GitHub's default when omitted");
        assert!(s.allow_rebase_merge);
        assert!(!s.delete_branch_on_merge);
        assert_eq!(s.merge_method(), MergeMethod::Merge);
        let _ = server.requests();
    }

    #[test]
    fn merge_method_prefers_squash_then_merge_then_rebase() {
        let mut s = RepoSettings::unknown("main");
        assert_eq!(s.merge_method(), MergeMethod::Squash);
        s.allow_squash_merge = false;
        assert_eq!(s.merge_method(), MergeMethod::Merge);
        s.allow_merge_commit = false;
        assert_eq!(s.merge_method(), MergeMethod::Rebase);
        s.allow_rebase_merge = false;
        assert_eq!(
            s.merge_method(),
            MergeMethod::Squash,
            "nothing allowed: still try"
        );
        assert_eq!(MergeMethod::Squash.rest(), "squash");
        assert_eq!(MergeMethod::Rebase.graphql(), "REBASE");
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
            MockResponse::json(200, r#"{"name":"task/5","protected":false}"#),
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
