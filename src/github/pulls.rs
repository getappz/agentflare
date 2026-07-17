use crate::github::models::PullRequest;
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
    let json = client.request("GET", &path, None)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_body_includes_optional_body_only_when_present() {
        let with = create_body("t", "h", "b", Some("desc"));
        assert_eq!(with["title"], "t");
        assert_eq!(with["body"], "desc");
        let without = create_body("t", "h", "b", None);
        assert!(without.get("body").is_none());
    }
}
