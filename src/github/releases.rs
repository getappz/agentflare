//! Release operations. Same shape as `pulls`/`issues`.

use crate::github::models::Release;
use crate::github::{Client, GitHubError, RepoId};

fn create_body(
    tag: &str,
    name: Option<&str>,
    body: Option<&str>,
    draft: bool,
    prerelease: bool,
) -> serde_json::Value {
    let mut v = serde_json::json!({ "tag_name": tag, "draft": draft, "prerelease": prerelease });
    if let Some(n) = name {
        v["name"] = serde_json::Value::String(n.to_string());
    }
    if let Some(b) = body {
        v["body"] = serde_json::Value::String(b.to_string());
    }
    v
}

pub fn list(client: &Client, repo: &RepoId) -> Result<Vec<Release>, GitHubError> {
    let path = format!("/repos/{}/{}/releases", repo.owner, repo.repo);
    let json = client.get_paginated(&path, crate::github::client::as_array)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn get(client: &Client, repo: &RepoId, id: u64) -> Result<Release, GitHubError> {
    let path = format!("/repos/{}/{}/releases/{id}", repo.owner, repo.repo);
    let json = client.request("GET", &path, None)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn latest(client: &Client, repo: &RepoId) -> Result<Release, GitHubError> {
    let path = format!("/repos/{}/{}/releases/latest", repo.owner, repo.repo);
    let json = client.request("GET", &path, None)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn create(
    client: &Client,
    repo: &RepoId,
    tag: &str,
    name: Option<&str>,
    body: Option<&str>,
    draft: bool,
    prerelease: bool,
) -> Result<Release, GitHubError> {
    let path = format!("/repos/{}/{}/releases", repo.owner, repo.repo);
    let json = client.request(
        "POST",
        &path,
        Some(create_body(tag, name, body, draft, prerelease)),
    )?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_body_sets_flags_and_optional_names() {
        let v = create_body("v1.0.0", Some("First"), Some("notes"), true, false);
        assert_eq!(v["tag_name"], "v1.0.0");
        assert_eq!(v["name"], "First");
        assert_eq!(v["body"], "notes");
        assert_eq!(v["draft"], true);
        assert_eq!(v["prerelease"], false);

        let bare = create_body("v1.0.0", None, None, false, false);
        assert!(bare.get("name").is_none());
        assert!(bare.get("body").is_none());
    }

    use crate::github::test_support::{MockResponse, MockServer};

    fn repo() -> RepoId {
        RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    const REL: &str = r#"{"id":1,"tag_name":"v1.0.0","html_url":"u"}"#;

    #[test]
    fn list_gets_releases() {
        let server = MockServer::start(vec![MockResponse::json(200, "[]")]);
        let client = server.client(None);
        assert!(list(&client, &repo()).unwrap().is_empty());
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/releases?per_page=100&page=1"
        );
    }

    #[test]
    fn get_fetches_by_id() {
        let server = MockServer::start(vec![MockResponse::json(200, REL)]);
        let client = server.client(None);
        let rel = get(&client, &repo(), 1).unwrap();
        assert_eq!(rel.tag_name, "v1.0.0");
        assert_eq!(server.requests()[0].path, "/repos/o/r/releases/1");
    }

    #[test]
    fn latest_hits_the_latest_endpoint() {
        let server = MockServer::start(vec![MockResponse::json(200, REL)]);
        let client = server.client(None);
        latest(&client, &repo()).unwrap();
        assert_eq!(server.requests()[0].path, "/repos/o/r/releases/latest");
    }

    #[test]
    fn create_posts_release_body() {
        let server = MockServer::start(vec![MockResponse::json(201, REL)]);
        let client = server.client(Some("tok"));
        create(&client, &repo(), "v1.0.0", Some("First"), None, true, false).unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/repos/o/r/releases");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["tag_name"], "v1.0.0");
        assert_eq!(sent["draft"], true);
        assert_eq!(sent["name"], "First");
    }
}
