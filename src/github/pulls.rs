use crate::github::models::{PullRequest, Review, ReviewComment};
use crate::github::{Client, GitHubError, RepoId};

fn create_body(title: &str, head: &str, base: &str, body: Option<&str>) -> serde_json::Value {
    let mut v = serde_json::json!({ "title": title, "head": head, "base": base });
    if let Some(b) = body {
        v["body"] = serde_json::Value::String(b.to_string());
    }
    v
}

pub fn create(
    client: &Client,
    repo: &RepoId,
    title: &str,
    head: &str,
    base: &str,
    body: Option<&str>,
) -> Result<PullRequest, GitHubError> {
    let path = format!("/repos/{}/{}/pulls", repo.owner, repo.repo);
    let json = client.request("POST", &path, Some(create_body(title, head, base, body)))?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn list(client: &Client, repo: &RepoId, state: &str) -> Result<Vec<PullRequest>, GitHubError> {
    let path = format!(
        "/repos/{}/{}/pulls?state={}",
        repo.owner,
        repo.repo,
        crate::github::encode_query(state)
    );
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn get(client: &Client, repo: &RepoId, number: u64) -> Result<PullRequest, GitHubError> {
    let path = format!("/repos/{}/{}/pulls/{number}", repo.owner, repo.repo);
    let json = client.request("GET", &path, None)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn merge(client: &Client, repo: &RepoId, number: u64, method: &str) -> Result<(), GitHubError> {
    let path = format!("/repos/{}/{}/pulls/{number}/merge", repo.owner, repo.repo);
    client.request(
        "PUT",
        &path,
        Some(serde_json::json!({ "merge_method": method })),
    )?;
    Ok(())
}

pub fn comment(client: &Client, repo: &RepoId, number: u64, body: &str) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/issues/{number}/comments",
        repo.owner, repo.repo
    );
    client.request("POST", &path, Some(serde_json::json!({ "body": body })))?;
    Ok(())
}

pub fn request_review(
    client: &Client,
    repo: &RepoId,
    number: u64,
    reviewers: &[String],
) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/pulls/{number}/requested_reviewers",
        repo.owner, repo.repo
    );
    client.request(
        "POST",
        &path,
        Some(serde_json::json!({ "reviewers": reviewers })),
    )?;
    Ok(())
}

/// Review verdicts (approve/request-changes/comment) — includes bot reviewers
/// like CodeRabbit, which submit as ordinary PR reviews.
pub fn list_reviews(
    client: &Client,
    repo: &RepoId,
    number: u64,
) -> Result<Vec<Review>, GitHubError> {
    let path = format!("/repos/{}/{}/pulls/{number}/reviews", repo.owner, repo.repo);
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

/// Line-anchored review comments (diff comments), separate from general
/// issue-style comments returned by `issues::list_comments`. `since` (an
/// ISO8601 timestamp) filters to comments created after it — GitHub applies
/// the filter server-side, so passing the last-checked time keeps repeated
/// `pr_status` calls cheap.
pub fn list_review_comments(
    client: &Client,
    repo: &RepoId,
    number: u64,
    since: Option<&str>,
) -> Result<Vec<ReviewComment>, GitHubError> {
    let mut path = format!(
        "/repos/{}/{}/pulls/{number}/comments",
        repo.owner, repo.repo
    );
    if let Some(s) = since {
        path.push_str(&format!("?since={}", crate::github::encode_query(s)));
    }
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

/// Database IDs of review comments belonging to a *resolved* review thread.
/// REST has no resolution field at all (only GraphQL's `reviewThread.isResolved`
/// does), so this is a separate GraphQL call whose only job is to produce an
/// id set that `pr_status` filters the REST comment list against.
///
/// ponytail: caps at the first 100 threads / 50 comments per thread (no
/// cursor pagination) — plenty for a normal PR; revisit if a PR ever has more.
pub fn resolved_review_comment_ids(
    client: &Client,
    repo: &RepoId,
    number: u64,
) -> Result<std::collections::HashSet<u64>, GitHubError> {
    const QUERY: &str = "query($owner:String!,$repo:String!,$number:Int!){repository(owner:$owner,name:$repo){pullRequest(number:$number){reviewThreads(first:100){nodes{isResolved comments(first:50){nodes{databaseId}}}}}}}";
    let body = serde_json::json!({
        "query": QUERY,
        "variables": { "owner": repo.owner, "repo": repo.repo, "number": number }
    });
    let json = client.request("POST", "/graphql", Some(body))?;
    if let Some(errors) = json.get("errors") {
        return Err(GitHubError::Parse(format!("GraphQL error: {errors}")));
    }
    let threads = json["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut resolved = std::collections::HashSet::new();
    for t in &threads {
        if !t["isResolved"].as_bool().unwrap_or(false) {
            continue;
        }
        if let Some(comments) = t["comments"]["nodes"].as_array() {
            for c in comments {
                if let Some(id) = c["databaseId"].as_u64() {
                    resolved.insert(id);
                }
            }
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::github::test_support::{MockResponse, MockServer};

    #[test]
    fn create_body_includes_optional_body_only_when_present() {
        let with = create_body("t", "h", "b", Some("desc"));
        assert_eq!(with["title"], "t");
        assert_eq!(with["body"], "desc");
        let without = create_body("t", "h", "b", None);
        assert!(without.get("body").is_none());
    }

    fn repo() -> RepoId {
        RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    #[test]
    fn create_posts_to_pulls_and_parses_the_response() {
        let server = MockServer::start(vec![MockResponse::json(
            201,
            r#"{"number":7,"html_url":"https://gh/o/r/pull/7","state":"open","title":"t"}"#,
        )]);
        let client = server.client(Some("tok"));
        let pr = create(&client, &repo(), "t", "head", "main", Some("desc")).unwrap();
        assert_eq!(pr.number, 7);

        let reqs = server.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/repos/o/r/pulls");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["head"], "head");
        assert_eq!(sent["base"], "main");
        assert_eq!(sent["body"], "desc");
    }

    #[test]
    fn list_encodes_state_in_the_query() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"number":1,"html_url":"u","state":"open","title":"a"}]"#,
        )]);
        let client = server.client(None);
        let prs = list(&client, &repo(), "open").unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/pulls?state=open&per_page=100&page=1"
        );
    }

    #[test]
    fn get_fetches_a_single_pull() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"number":9,"html_url":"u","state":"closed","title":"x"}"#,
        )]);
        let client = server.client(None);
        let pr = get(&client, &repo(), 9).unwrap();
        assert_eq!(pr.state, "closed");
        assert_eq!(server.requests()[0].path, "/repos/o/r/pulls/9");
    }

    #[test]
    fn merge_puts_the_chosen_method() {
        let server = MockServer::start(vec![MockResponse::json(200, r#"{"merged":true}"#)]);
        let client = server.client(Some("tok"));
        merge(&client, &repo(), 3, "squash").unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "PUT");
        assert_eq!(reqs[0].path, "/repos/o/r/pulls/3/merge");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["merge_method"], "squash");
    }

    #[test]
    fn comment_posts_to_the_issues_comments_endpoint() {
        let server = MockServer::start(vec![MockResponse::json(201, r#"{"id":1}"#)]);
        let client = server.client(Some("tok"));
        comment(&client, &repo(), 5, "hello").unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/repos/o/r/issues/5/comments");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["body"], "hello");
    }

    #[test]
    fn request_review_sends_the_reviewer_list() {
        let server = MockServer::start(vec![MockResponse::json(201, r#"{"id":1}"#)]);
        let client = server.client(Some("tok"));
        request_review(&client, &repo(), 8, &["alice".into(), "bob".into()]).unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/repos/o/r/pulls/8/requested_reviewers");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["reviewers"][0], "alice");
        assert_eq!(sent["reviewers"][1], "bob");
    }

    #[test]
    fn list_reviews_fetches_and_parses() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"user":{"login":"coderabbitai[bot]"},"state":"APPROVED","body":"lgtm","submitted_at":"2026-07-19T00:00:00Z"}]"#,
        )]);
        let client = server.client(None);
        let reviews = list_reviews(&client, &repo(), 5).unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].user.login, "coderabbitai[bot]");
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/pulls/5/reviews?per_page=100&page=1"
        );
    }

    #[test]
    fn list_review_comments_fetches_and_parses() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"id":1,"user":{"login":"bob"},"path":"src/x.rs","line":42,"body":"nit"}]"#,
        )]);
        let client = server.client(None);
        let comments = list_review_comments(&client, &repo(), 5, None).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].line, Some(42));
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/pulls/5/comments?per_page=100&page=1"
        );
    }

    #[test]
    fn list_review_comments_appends_since_query() {
        let server = MockServer::start(vec![MockResponse::json(200, "[]")]);
        let client = server.client(None);
        list_review_comments(&client, &repo(), 5, Some("2026-07-19T00:00:00Z")).unwrap();
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/pulls/5/comments?since=2026-07-19T00%3A00%3A00Z&per_page=100&page=1"
        );
    }

    #[test]
    fn resolved_review_comment_ids_collects_ids_from_resolved_threads_only() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
                {"isResolved":true,"comments":{"nodes":[{"databaseId":1},{"databaseId":2}]}},
                {"isResolved":false,"comments":{"nodes":[{"databaseId":3}]}}
            ]}}}}}"#,
        )]);
        let client = server.client(Some("tok"));
        let ids = resolved_review_comment_ids(&client, &repo(), 5).unwrap();
        assert!(ids.contains(&1));
        assert!(ids.contains(&2));
        assert!(!ids.contains(&3));
        assert_eq!(server.requests()[0].path, "/graphql");
    }

    #[test]
    fn resolved_review_comment_ids_surfaces_graphql_errors() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"errors":[{"message":"Could not resolve to a PullRequest"}]}"#,
        )]);
        let client = server.client(Some("tok"));
        let err = resolved_review_comment_ids(&client, &repo(), 5).unwrap_err();
        assert!(matches!(err, GitHubError::Parse(_)));
    }
}
