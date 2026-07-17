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
}
