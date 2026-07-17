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
        assert_eq!(server.requests()[0].path, "/repos/o/r/pulls?state=open");
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
}
