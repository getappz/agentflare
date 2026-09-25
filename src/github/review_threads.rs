//! Review-thread level access to a PR's line comments: the GraphQL thread
//! query (with resolution state, which REST never exposes), the REST reply
//! endpoint, and the `resolveReviewThread` mutation. `supervisor::review_bots`
//! drives CodeRabbit-style bot threads through these -- fix, reply naming
//! the commit, resolve -- so nothing here knows what a "finding" is; it only
//! moves comments and resolution bits.
//!
//! Every call goes through `Client::graphql` / `Client::request`, so the
//! rate-limit backoff and mutation spacing those apply cover this file too.

use crate::github::{Client, GitHubError, RepoId};

/// One comment inside a review thread, as GraphQL reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadComment {
    /// REST id (`databaseId`): what the replies endpoint addresses.
    pub database_id: u64,
    pub login: String,
    pub body: String,
    /// ISO8601 `createdAt`; lexicographic order is chronological order.
    pub created_at: String,
}

/// A review thread: one line-anchored conversation on the PR diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewThread {
    /// GraphQL node id (`PRRT_...`): what `resolveReviewThread` addresses.
    pub id: String,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub path: String,
    pub line: Option<u64>,
    /// Root comment first, replies after, in creation order.
    pub comments: Vec<ThreadComment>,
}

impl ReviewThread {
    pub fn root(&self) -> Option<&ThreadComment> {
        self.comments.first()
    }
}

const THREADS_QUERY: &str = "query($owner:String!,$repo:String!,$number:Int!,$after:String){repository(owner:$owner,name:$repo){pullRequest(number:$number){reviewThreads(first:100,after:$after){pageInfo{hasNextPage endCursor} nodes{id isResolved isOutdated path line comments(first:50){nodes{databaseId author{login} body createdAt}}}}}}}";

/// Every review thread on `number`, following `reviewThreads` pagination
/// until GitHub reports no further page. Comments per thread are capped at
/// the first 50 -- a bot thread that long is a conversation a human should
/// be reading anyway.
pub fn list_review_threads(
    client: &Client,
    repo: &RepoId,
    number: u64,
) -> Result<Vec<ReviewThread>, GitHubError> {
    let mut threads = Vec::new();
    let mut after: Option<String> = None;
    // Bounded so a mock or a misbehaving endpoint that keeps reporting a
    // next page can't spin forever: 50 pages is 5,000 threads.
    for _ in 0..50 {
        let body = serde_json::json!({
            "query": THREADS_QUERY,
            "variables": {
                "owner": repo.owner, "repo": repo.repo, "number": number, "after": after,
            }
        });
        let json = client.graphql(body)?;
        if let Some(errors) = json.get("errors") {
            return Err(crate::github::graphql::graphql_error(client, errors));
        }
        let page = &json["data"]["repository"]["pullRequest"]["reviewThreads"];
        threads.extend(
            page["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(parse_thread),
        );
        let has_next = page["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false);
        let cursor = page["pageInfo"]["endCursor"].as_str().map(str::to_string);
        match (has_next, cursor) {
            (true, Some(c)) => after = Some(c),
            _ => break,
        }
    }
    Ok(threads)
}

fn parse_thread(node: &serde_json::Value) -> ReviewThread {
    let comments = node["comments"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| ThreadComment {
            database_id: c["databaseId"].as_u64().unwrap_or(0),
            login: c["author"]["login"].as_str().unwrap_or("").to_string(),
            body: c["body"].as_str().unwrap_or("").to_string(),
            created_at: c["createdAt"].as_str().unwrap_or("").to_string(),
        })
        .collect();
    ReviewThread {
        id: node["id"].as_str().unwrap_or("").to_string(),
        is_resolved: node["isResolved"].as_bool().unwrap_or(false),
        is_outdated: node["isOutdated"].as_bool().unwrap_or(false),
        path: node["path"].as_str().unwrap_or("").to_string(),
        line: node["line"].as_u64(),
        comments,
    }
}

/// Database ids of every comment in a resolved thread -- the id set
/// `pr_status` filters the REST comment list against (REST has no
/// resolution field at all).
pub fn resolved_review_comment_ids(
    client: &Client,
    repo: &RepoId,
    number: u64,
) -> Result<std::collections::HashSet<u64>, GitHubError> {
    Ok(list_review_threads(client, repo, number)?
        .into_iter()
        .filter(|t| t.is_resolved)
        .flat_map(|t| t.comments.into_iter().map(|c| c.database_id))
        .collect())
}

/// Replies on the thread rooted at review comment `comment_id`
/// (`POST /pulls/{number}/comments/{comment_id}/replies`); returns the new
/// comment's id.
pub fn reply_to_review_comment(
    client: &Client,
    repo: &RepoId,
    number: u64,
    comment_id: u64,
    body: &str,
) -> Result<u64, GitHubError> {
    let path = format!(
        "/repos/{}/{}/pulls/{number}/comments/{comment_id}/replies",
        repo.owner, repo.repo
    );
    let json = client.request("POST", &path, Some(serde_json::json!({ "body": body })))?;
    json["id"]
        .as_u64()
        .ok_or_else(|| GitHubError::Parse("reply response had no id".to_string()))
}

/// Marks thread `thread_id` (a GraphQL node id) resolved.
pub fn resolve_review_thread(client: &Client, thread_id: &str) -> Result<(), GitHubError> {
    const MUTATION: &str =
        "mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{id isResolved}}}";
    let json = client.graphql(serde_json::json!({
        "query": MUTATION,
        "variables": { "id": thread_id },
    }))?;
    if let Some(errors) = json.get("errors") {
        return Err(crate::github::graphql::graphql_error(client, errors));
    }
    Ok(())
}

/// Replies on a thread and then, only once the reply has landed, resolves
/// it. The order is the point: a thread resolved without its reply reads as
/// silently dismissed, while a reply without the resolve is merely untidy
/// and the next sweep finishes the job. Returns the reply's id.
pub fn reply_then_resolve(
    client: &Client,
    repo: &RepoId,
    number: u64,
    thread: &ReviewThread,
    body: &str,
) -> Result<u64, GitHubError> {
    let root = thread
        .root()
        .ok_or_else(|| GitHubError::Parse("thread has no root comment".to_string()))?;
    let reply_id = reply_to_review_comment(client, repo, number, root.database_id, body)?;
    resolve_review_thread(client, &thread.id)?;
    Ok(reply_id)
}

/// Every commit sha on the PR, oldest first -- how a sweep confirms an
/// agent's "fixed in <sha>" actually reached the remote before it replies.
pub fn pr_commit_shas(
    client: &Client,
    repo: &RepoId,
    number: u64,
) -> Result<Vec<String>, GitHubError> {
    let path = format!("/repos/{}/{}/pulls/{number}/commits", repo.owner, repo.repo);
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    Ok(json
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["sha"].as_str().map(str::to_string))
        .collect())
}

/// A legacy commit status with its human-readable description kept --
/// `actions::list_commit_statuses` folds these into `CheckRun`s and drops
/// the description, but a review bot's pause state lives in exactly that
/// field (`CodeRabbit: Review paused`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitStatusDetail {
    pub context: String,
    pub state: String,
    pub description: String,
}

pub fn list_commit_status_details(
    client: &Client,
    repo: &RepoId,
    sha: &str,
) -> Result<Vec<CommitStatusDetail>, GitHubError> {
    let path = format!(
        "/repos/{}/{}/commits/{sha}/status?per_page=100",
        repo.owner, repo.repo
    );
    let json = client.request("GET", &path, None)?;
    Ok(json["statuses"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|st| {
            Some(CommitStatusDetail {
                context: st["context"].as_str()?.to_string(),
                state: st["state"].as_str().unwrap_or_default().to_string(),
                description: st["description"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect())
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

    fn thread_page(nodes: &str, has_next: bool, cursor: &str) -> String {
        format!(
            r#"{{"data":{{"repository":{{"pullRequest":{{"reviewThreads":{{
                "pageInfo":{{"hasNextPage":{has_next},"endCursor":"{cursor}"}},
                "nodes":[{nodes}]}}}}}}}}}}"#
        )
    }

    const ROOT_THREAD: &str = r#"{"id":"PRRT_1","isResolved":false,"isOutdated":false,"path":"src/x.rs","line":42,
        "comments":{"nodes":[
            {"databaseId":11,"author":{"login":"coderabbitai[bot]"},"body":"finding","createdAt":"2026-09-01T00:00:00Z"},
            {"databaseId":12,"author":{"login":"me"},"body":"reply","createdAt":"2026-09-01T01:00:00Z"}
        ]}}"#;

    #[test]
    fn list_review_threads_follows_pagination_and_parses_every_field() {
        let page_two = r#"{"id":"PRRT_2","isResolved":true,"isOutdated":true,"path":"src/y.rs","line":null,"comments":{"nodes":[]}}"#;
        let server = MockServer::start(vec![
            MockResponse::json(200, &thread_page(ROOT_THREAD, true, "cur1")),
            MockResponse::json(200, &thread_page(page_two, false, "cur2")),
        ]);
        let client = server.client(Some("tok"));
        let threads = list_review_threads(&client, &repo(), 5).unwrap();
        assert_eq!(threads.len(), 2);
        let first = &threads[0];
        assert_eq!(first.id, "PRRT_1");
        assert_eq!(first.path, "src/x.rs");
        assert_eq!(first.line, Some(42));
        assert!(!first.is_resolved && !first.is_outdated);
        assert_eq!(first.root().unwrap().login, "coderabbitai[bot]");
        assert_eq!(first.comments.last().unwrap().database_id, 12);
        assert!(threads[1].is_resolved && threads[1].is_outdated);
        assert_eq!(threads[1].line, None);

        let reqs = server.requests();
        assert_eq!(reqs.len(), 2, "one call per page");
        let first_vars: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert!(first_vars["variables"]["after"].is_null());
        let second_vars: serde_json::Value = serde_json::from_str(&reqs[1].body).unwrap();
        assert_eq!(second_vars["variables"]["after"], "cur1");
    }

    #[test]
    fn list_review_threads_tolerates_a_page_without_page_info() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"repository":{"pullRequest":{"reviewThreads":{"nodes":[
                {"isResolved":true,"comments":{"nodes":[{"databaseId":1}]}}
            ]}}}}}"#,
        )]);
        let client = server.client(Some("tok"));
        let ids = resolved_review_comment_ids(&client, &repo(), 5).unwrap();
        assert!(ids.contains(&1));
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn list_review_threads_surfaces_graphql_errors() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"errors":[{"message":"Could not resolve to a PullRequest"}]}"#,
        )]);
        let client = server.client(Some("tok"));
        let err = list_review_threads(&client, &repo(), 5).unwrap_err();
        assert!(matches!(err, GitHubError::Parse(_)));
    }

    #[test]
    fn reply_posts_to_the_replies_endpoint_and_returns_the_id() {
        let server = MockServer::start(vec![MockResponse::json(201, r#"{"id":77}"#)]);
        let client = server.client(Some("tok"));
        let id = reply_to_review_comment(&client, &repo(), 5, 11, "Fixed in abc.").unwrap();
        assert_eq!(id, 77);
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/repos/o/r/pulls/5/comments/11/replies");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["body"], "Fixed in abc.");
    }

    #[test]
    fn resolve_sends_the_mutation_with_the_thread_id() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"resolveReviewThread":{"thread":{"id":"PRRT_1","isResolved":true}}}}"#,
        )]);
        let client = server.client(Some("tok"));
        resolve_review_thread(&client, "PRRT_1").unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/graphql");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert!(
            sent["query"].as_str().unwrap().starts_with("mutation"),
            "must be spaced as a mutation"
        );
        assert_eq!(sent["variables"]["id"], "PRRT_1");
    }

    #[test]
    fn reply_then_resolve_replies_first_and_resolves_second() {
        let server = MockServer::start(vec![
            MockResponse::json(201, r#"{"id":78}"#),
            MockResponse::json(200, r#"{"data":{"resolveReviewThread":{"thread":{}}}}"#),
        ]);
        let client = server.client(Some("tok"));
        let thread = parse_thread(&serde_json::from_str(ROOT_THREAD).unwrap());
        let id = reply_then_resolve(&client, &repo(), 5, &thread, "Fixed in abc.").unwrap();
        assert_eq!(id, 78);
        let reqs = server.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].path, "/repos/o/r/pulls/5/comments/11/replies");
        assert_eq!(reqs[1].path, "/graphql");
    }

    #[test]
    fn reply_then_resolve_never_resolves_when_the_reply_fails() {
        let server = MockServer::start(vec![MockResponse::json(
            422,
            r#"{"message":"Validation Failed"}"#,
        )]);
        let client = server.client(Some("tok"));
        let thread = parse_thread(&serde_json::from_str(ROOT_THREAD).unwrap());
        assert!(reply_then_resolve(&client, &repo(), 5, &thread, "x").is_err());
        assert_eq!(
            server.requests().len(),
            1,
            "a failed reply must not be followed by a resolve"
        );
    }

    #[test]
    fn pr_commit_shas_lists_every_sha_in_order() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"sha":"aaa111"},{"sha":"bbb222"}]"#,
        )]);
        let client = server.client(None);
        let shas = pr_commit_shas(&client, &repo(), 5).unwrap();
        assert_eq!(shas, vec!["aaa111", "bbb222"]);
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/pulls/5/commits?per_page=100&page=1"
        );
    }

    #[test]
    fn commit_status_details_keep_the_description() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"state":"pending","statuses":[
                {"context":"CodeRabbit","state":"pending","description":"Review paused"},
                {"context":"ci","state":"success"}
            ]}"#,
        )]);
        let client = server.client(None);
        let statuses = list_commit_status_details(&client, &repo(), "abc").unwrap();
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].description, "Review paused");
        assert_eq!(statuses[1].description, "");
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/commits/abc/status?per_page=100"
        );
    }
}
