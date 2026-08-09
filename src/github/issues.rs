//! Issue operations. Same shape as `pulls`: build a REST path (+ body) and
//! delegate to `Client::request`, returning typed models.
//!
//! Note: GitHub's issues endpoint also returns pull requests (a PR is an
//! issue); `list` does not filter them out.

use crate::github::models::{Comment, Issue};
use crate::github::{Client, GitHubError, RepoId};

fn create_body(
    title: &str,
    body: Option<&str>,
    labels: &[String],
    assignees: &[String],
) -> serde_json::Value {
    let mut v = serde_json::json!({ "title": title });
    if let Some(b) = body {
        v["body"] = serde_json::Value::String(b.to_string());
    }
    if !labels.is_empty() {
        v["labels"] = serde_json::json!(labels);
    }
    if !assignees.is_empty() {
        v["assignees"] = serde_json::json!(assignees);
    }
    v
}

pub fn create(
    client: &Client,
    repo: &RepoId,
    title: &str,
    body: Option<&str>,
    labels: &[String],
    assignees: &[String],
) -> Result<Issue, GitHubError> {
    let path = format!("/repos/{}/{}/issues", repo.owner, repo.repo);
    let json = client.request(
        "POST",
        &path,
        Some(create_body(title, body, labels, assignees)),
    )?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn list(client: &Client, repo: &RepoId, state: &str) -> Result<Vec<Issue>, GitHubError> {
    let path = format!(
        "/repos/{}/{}/issues?state={}",
        repo.owner,
        repo.repo,
        crate::github::encode_query(state)
    );
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

/// Like [`list`], plus GitHub's server-side `labels` and `since` filters.
/// `since` is an ISO8601 timestamp compared against each issue's
/// `updated_at`.
///
/// The bridge deliberately passes `None`: it needs the WHOLE queue every
/// tick to decide what is claimable, and a `since`-filtered listing would
/// hide exactly the untouched issues that are most available. The parameter
/// stays for callers that want a delta, not as an optimization this one is
/// missing out on.
#[allow(dead_code)]
pub fn list_filtered(
    client: &Client,
    repo: &RepoId,
    state: &str,
    labels: Option<&str>,
    since: Option<&str>,
) -> Result<Vec<Issue>, GitHubError> {
    let mut path = format!(
        "/repos/{}/{}/issues?state={}",
        repo.owner,
        repo.repo,
        crate::github::encode_query(state)
    );
    if let Some(l) = labels {
        path.push_str(&format!("&labels={}", crate::github::encode_query(l)));
    }
    if let Some(s) = since {
        path.push_str(&format!("&since={}", crate::github::encode_query(s)));
    }
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn get(client: &Client, repo: &RepoId, number: u64) -> Result<Issue, GitHubError> {
    let path = format!("/repos/{}/{}/issues/{number}", repo.owner, repo.repo);
    let json = client.request("GET", &path, None)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

/// Posts a comment and returns its id, so a caller that needs to amend what
/// it just wrote (the bridge rewrites its claim marker with the item id it
/// only learns after the post) can do so without re-listing every comment.
pub fn comment(
    client: &Client,
    repo: &RepoId,
    number: u64,
    body: &str,
) -> Result<u64, GitHubError> {
    let path = format!(
        "/repos/{}/{}/issues/{number}/comments",
        repo.owner, repo.repo
    );
    let json = client.request("POST", &path, Some(serde_json::json!({ "body": body })))?;
    json["id"]
        .as_u64()
        .ok_or_else(|| GitHubError::Parse("comment response had no id".to_string()))
}

/// Rewrites an existing comment in place. The comment id is repo-scoped, not
/// issue-scoped, so no issue number is needed.
///
/// The bridge's heartbeat depends on this: refreshing a claim's marker by
/// editing the comment that carries it keeps exactly one marker per claim,
/// where posting a fresh one every half-TTL would bury the issue under
/// dozens of bookkeeping comments a human then has to scroll past.
pub fn update_comment(
    client: &Client,
    repo: &RepoId,
    comment_id: u64,
    body: &str,
) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/issues/comments/{comment_id}",
        repo.owner, repo.repo
    );
    client.request("PATCH", &path, Some(serde_json::json!({ "body": body })))?;
    Ok(())
}

/// General (non-line-anchored) comments — where bots like CodeRabbit post
/// their PR summary/walkthrough (a PR is also an issue on this endpoint).
/// `since` (ISO8601) filters server-side, same as `pulls::list_review_comments`.
pub fn list_comments(
    client: &Client,
    repo: &RepoId,
    number: u64,
    since: Option<&str>,
) -> Result<Vec<Comment>, GitHubError> {
    let mut path = format!(
        "/repos/{}/{}/issues/{number}/comments",
        repo.owner, repo.repo
    );
    if let Some(s) = since {
        path.push_str(&format!("?since={}", crate::github::encode_query(s)));
    }
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

/// All general (non-line-anchored) comments across the WHOLE repo in one
/// paginated listing, each tagged with its `issue_url` (see
/// [`Comment::issue_number`]) -- the repo-wide counterpart to
/// [`list_comments`]'s per-issue endpoint.
///
/// `queue_status` uses this instead of one [`list_comments`] call per open
/// issue: a queue of N issues used to cost N+1 requests (plus pagination on
/// each), which can exhaust rate limits or time out on a large queue. This
/// costs one paginated listing regardless of N, at the price of also
/// fetching comments on issues outside the queue label -- an acceptable
/// trade since the queue label already keeps `open_issues` itself small, and
/// callers filter by [`Comment::issue_number`] anyway.
pub fn list_all_comments(
    client: &Client,
    repo: &RepoId,
    since: Option<&str>,
) -> Result<Vec<Comment>, GitHubError> {
    let mut path = format!("/repos/{}/{}/issues/comments", repo.owner, repo.repo);
    if let Some(s) = since {
        path.push_str(&format!("?since={}", crate::github::encode_query(s)));
    }
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn close(client: &Client, repo: &RepoId, number: u64) -> Result<Issue, GitHubError> {
    let path = format!("/repos/{}/{}/issues/{number}", repo.owner, repo.repo);
    let json = client.request(
        "PATCH",
        &path,
        Some(serde_json::json!({ "state": "closed" })),
    )?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn add_labels(
    client: &Client,
    repo: &RepoId,
    number: u64,
    labels: &[String],
) -> Result<(), GitHubError> {
    let path = format!("/repos/{}/{}/issues/{number}/labels", repo.owner, repo.repo);
    client.request("POST", &path, Some(serde_json::json!({ "labels": labels })))?;
    Ok(())
}

/// Deletes a comment. Like [`remove_label`], an already-absent target is
/// treated as success. Repo-scoped comment id, same as [`update_comment`].
///
/// Test-gated: the bridge never deletes anything in production — it only
/// appends and edits, so its history stays auditable. This exists so the
/// live-GitHub harness can reset a scratch repo between runs. Drop the
/// `cfg(test)` if a production caller ever needs it.
#[cfg(test)]
pub fn delete_comment(client: &Client, repo: &RepoId, comment_id: u64) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/issues/comments/{comment_id}",
        repo.owner, repo.repo
    );
    match client.request("DELETE", &path, None) {
        Ok(_) | Err(GitHubError::NotFound) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Reopens a closed issue — the inverse of [`close`]. Test-gated for the same
/// reason as [`delete_comment`]: the bridge closes issues, never reopens them.
#[cfg(test)]
pub fn reopen(client: &Client, repo: &RepoId, number: u64) -> Result<Issue, GitHubError> {
    let path = format!("/repos/{}/{}/issues/{number}", repo.owner, repo.repo);
    let json = client.request("PATCH", &path, Some(serde_json::json!({ "state": "open" })))?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

/// Removes one label. A label that is not on the issue answers 404, which
/// callers generally want to treat as success — the desired end state holds
/// either way.
pub fn remove_label(
    client: &Client,
    repo: &RepoId,
    number: u64,
    label: &str,
) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/issues/{number}/labels/{}",
        repo.owner,
        repo.repo,
        crate::github::encode_query(label)
    );
    match client.request("DELETE", &path, None) {
        Ok(_) | Err(GitHubError::NotFound) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_body_includes_optional_fields_only_when_present() {
        let full = create_body(
            "t",
            Some("desc"),
            &["bug".to_string()],
            &["alice".to_string()],
        );
        assert_eq!(full["title"], "t");
        assert_eq!(full["body"], "desc");
        assert_eq!(full["labels"][0], "bug");
        assert_eq!(full["assignees"][0], "alice");

        let minimal = create_body("t", None, &[], &[]);
        assert!(minimal.get("body").is_none());
        assert!(minimal.get("labels").is_none());
        assert!(minimal.get("assignees").is_none());
    }

    use crate::github::test_support::{MockResponse, MockServer};

    fn repo() -> RepoId {
        RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    #[test]
    fn create_posts_to_issues() {
        let server = MockServer::start(vec![MockResponse::json(
            201,
            r#"{"number":11,"html_url":"u","state":"open","title":"t"}"#,
        )]);
        let client = server.client(Some("tok"));
        let issue = create(&client, &repo(), "t", None, &["bug".into()], &[]).unwrap();
        assert_eq!(issue.number, 11);
        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/repos/o/r/issues");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["labels"][0], "bug");
    }

    #[test]
    fn list_encodes_state() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"number":1,"html_url":"u","state":"closed","title":"a"}]"#,
        )]);
        let client = server.client(None);
        let issues = list(&client, &repo(), "closed").unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/issues?state=closed&per_page=100&page=1"
        );
    }

    #[test]
    fn get_fetches_single_issue() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"number":3,"html_url":"u","state":"open","title":"x"}"#,
        )]);
        let client = server.client(None);
        let issue = get(&client, &repo(), 3).unwrap();
        assert_eq!(issue.number, 3);
        assert_eq!(server.requests()[0].path, "/repos/o/r/issues/3");
    }

    #[test]
    fn comment_posts_body_and_returns_the_new_comment_id() {
        let server = MockServer::start(vec![MockResponse::json(201, r#"{"id":42}"#)]);
        let client = server.client(Some("tok"));
        assert_eq!(comment(&client, &repo(), 4, "hi").unwrap(), 42);
        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/repos/o/r/issues/4/comments");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["body"], "hi");
    }

    #[test]
    fn delete_comment_and_reopen_hit_the_right_endpoints() {
        let server = MockServer::start(vec![
            MockResponse::json(204, ""),
            MockResponse::json(404, r#"{"message":"Not Found"}"#),
            MockResponse::json(
                200,
                r#"{"number":4,"html_url":"u","state":"open","title":"t"}"#,
            ),
        ]);
        let client = server.client(Some("tok"));

        delete_comment(&client, &repo(), 9).unwrap();
        delete_comment(&client, &repo(), 9).expect("an already-deleted comment is success");
        assert_eq!(reopen(&client, &repo(), 4).unwrap().state, "open");

        let reqs = server.requests();
        assert_eq!(reqs[0].method, "DELETE");
        assert_eq!(reqs[0].path, "/repos/o/r/issues/comments/9");
        assert_eq!(reqs[2].method, "PATCH");
        let sent: serde_json::Value = serde_json::from_str(&reqs[2].body).unwrap();
        assert_eq!(sent["state"], "open");
    }

    #[test]
    fn remove_label_deletes_the_encoded_label_and_tolerates_absence() {
        // The label carries an instance id (`claimed:agent:host`), so the
        // colons must survive as path-safe encoding rather than as separators.
        let server = MockServer::start(vec![
            MockResponse::json(200, "[]"),
            MockResponse::json(404, r#"{"message":"Label does not exist"}"#),
        ]);
        let client = server.client(Some("tok"));

        remove_label(&client, &repo(), 4, "claimed:a:1").unwrap();
        remove_label(&client, &repo(), 4, "claimed:a:1")
            .expect("a label that is already gone is the desired end state");

        let reqs = server.requests();
        assert_eq!(reqs[0].method, "DELETE");
        assert_eq!(reqs[0].path, "/repos/o/r/issues/4/labels/claimed%3Aa%3A1");
    }

    #[test]
    fn update_comment_patches_the_repo_scoped_comment_endpoint() {
        let server = MockServer::start(vec![MockResponse::json(200, r#"{"id":9}"#)]);
        let client = server.client(Some("tok"));
        update_comment(&client, &repo(), 9, "refreshed").unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "PATCH");
        // Note: `/issues/comments/{id}`, NOT `/issues/{number}/comments/{id}`.
        assert_eq!(reqs[0].path, "/repos/o/r/issues/comments/9");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["body"], "refreshed");
    }

    #[test]
    fn list_comments_fetches_and_parses() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"id":1,"user":{"login":"coderabbitai[bot]"},"body":"walkthrough...","created_at":"2026-07-19T00:00:00Z"}]"#,
        )]);
        let client = server.client(None);
        let comments = list_comments(&client, &repo(), 4, None).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].user.login, "coderabbitai[bot]");
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/issues/4/comments?per_page=100&page=1"
        );
    }

    #[test]
    fn list_comments_appends_since_query() {
        let server = MockServer::start(vec![MockResponse::json(200, "[]")]);
        let client = server.client(None);
        list_comments(&client, &repo(), 4, Some("2026-07-19T00:00:00Z")).unwrap();
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/issues/4/comments?since=2026-07-19T00%3A00%3A00Z&per_page=100&page=1"
        );
    }

    #[test]
    fn list_all_comments_fetches_the_repo_wide_endpoint_with_issue_urls() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"id":1,"user":{"login":"a"},"body":"hi","issue_url":"https://api.github.com/repos/o/r/issues/7"}]"#,
        )]);
        let client = server.client(None);
        let comments = list_all_comments(&client, &repo(), None).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].issue_number(), Some(7));
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/issues/comments?per_page=100&page=1"
        );
    }

    #[test]
    fn list_all_comments_appends_since_query() {
        let server = MockServer::start(vec![MockResponse::json(200, "[]")]);
        let client = server.client(None);
        list_all_comments(&client, &repo(), Some("2026-07-19T00:00:00Z")).unwrap();
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/issues/comments?since=2026-07-19T00%3A00%3A00Z&per_page=100&page=1"
        );
    }

    #[test]
    fn close_patches_state_to_closed() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"number":6,"html_url":"u","state":"closed","title":"x"}"#,
        )]);
        let client = server.client(Some("tok"));
        let issue = close(&client, &repo(), 6).unwrap();
        assert_eq!(issue.state, "closed");
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "PATCH");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["state"], "closed");
    }

    #[test]
    fn add_labels_posts_the_label_list() {
        let server = MockServer::start(vec![MockResponse::json(200, "[]")]);
        let client = server.client(Some("tok"));
        add_labels(&client, &repo(), 2, &["a".into(), "b".into()]).unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/repos/o/r/issues/2/labels");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["labels"][1], "b");
    }

    #[test]
    fn list_filtered_encodes_labels_and_since() {
        let server = MockServer::start(vec![MockResponse::json(200, "[]")]);
        let client = server.client(None);
        list_filtered(
            &client,
            &repo(),
            "open",
            Some("agentflare"),
            Some("2026-08-03T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/issues?state=open&labels=agentflare&since=2026-08-03T00%3A00%3A00Z&per_page=100&page=1"
        );
    }

    #[test]
    fn list_filtered_omits_absent_filters() {
        let server = MockServer::start(vec![MockResponse::json(200, "[]")]);
        let client = server.client(None);
        list_filtered(&client, &repo(), "open", None, None).unwrap();
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/issues?state=open&per_page=100&page=1"
        );
    }
}
