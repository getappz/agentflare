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

pub fn get(client: &Client, repo: &RepoId, number: u64) -> Result<Issue, GitHubError> {
    let path = format!("/repos/{}/{}/issues/{number}", repo.owner, repo.repo);
    let json = client.request("GET", &path, None)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn comment(client: &Client, repo: &RepoId, number: u64, body: &str) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/issues/{number}/comments",
        repo.owner, repo.repo
    );
    client.request("POST", &path, Some(serde_json::json!({ "body": body })))?;
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
    fn comment_posts_body() {
        let server = MockServer::start(vec![MockResponse::json(201, r#"{"id":1}"#)]);
        let client = server.client(Some("tok"));
        comment(&client, &repo(), 4, "hi").unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/repos/o/r/issues/4/comments");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["body"], "hi");
    }

    #[test]
    fn list_comments_fetches_and_parses() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"[{"user":{"login":"coderabbitai[bot]"},"body":"walkthrough...","created_at":"2026-07-19T00:00:00Z"}]"#,
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
}
