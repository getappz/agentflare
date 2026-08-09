//! Git Contents/Refs API — just enough to read and write one file on a named
//! branch, creating that branch as an orphan commit (empty tree, no parent)
//! if it doesn't exist yet — the branch carries only what's written onto it
//! via `put_file`, not a snapshot of the whole repo. Backs `memory::sync`;
//! not a general git client.

use crate::github::{Client, GitHubError, RepoId};
use base64::Engine as _;

#[derive(Debug)]
pub struct FileContent {
    pub content: String,
    pub sha: String,
}

fn decode_content(json: &serde_json::Value) -> Result<String, GitHubError> {
    // For files over the Contents API's 1MB inline limit, GitHub returns
    // encoding "none" with an empty content string instead of the real
    // bytes -- silently decoding that as an empty file would make a sync
    // treat a too-large remote log as empty and overwrite it with the local
    // merge, destroying every remote-only fact in one push. Fail loud
    // instead: this API just isn't built to carry a file past that size.
    let encoding = json.get("encoding").and_then(|v| v.as_str()).unwrap_or("");
    if encoding != "base64" {
        return Err(GitHubError::Parse(format!(
            "contents response has encoding {encoding:?}, expected \"base64\" \
             (files over the Contents API's 1MB inline limit report empty content instead of erroring)"
        )));
    }
    let raw = json
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| GitHubError::Parse("contents response missing content".to_string()))?;
    // GitHub line-wraps the base64 payload at 60 chars.
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(compact)
        .map_err(|e| GitHubError::Parse(format!("contents response had bad base64: {e}")))?;
    String::from_utf8(bytes)
        .map_err(|e| GitHubError::Parse(format!("contents response was not utf8: {e}")))
}

/// Fetches `path` at `r#ref` (a branch name). `None` means the file does not
/// exist yet on that branch — the caller should treat that as an empty file.
pub fn get_file(
    client: &Client,
    repo: &RepoId,
    path: &str,
    r#ref: &str,
) -> Result<Option<FileContent>, GitHubError> {
    let url = format!(
        "/repos/{}/{}/contents/{}?ref={}",
        repo.owner,
        repo.repo,
        crate::github::encode_query(path),
        crate::github::encode_query(r#ref),
    );
    match client.request("GET", &url, None) {
        Ok(json) => {
            let sha = json
                .get("sha")
                .and_then(|v| v.as_str())
                .ok_or_else(|| GitHubError::Parse("contents response missing sha".to_string()))?
                .to_string();
            Ok(Some(FileContent {
                content: decode_content(&json)?,
                sha,
            }))
        }
        Err(GitHubError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Creates or updates `path` on `branch` in a single commit. `sha` is the
/// current file's sha (from `get_file`) when updating, `None` when creating.
/// Returns the new sha.
pub fn put_file(
    client: &Client,
    repo: &RepoId,
    path: &str,
    branch: &str,
    message: &str,
    content: &str,
    sha: Option<&str>,
) -> Result<String, GitHubError> {
    let url = format!(
        "/repos/{}/{}/contents/{}",
        repo.owner,
        repo.repo,
        crate::github::encode_query(path)
    );
    let encoded = base64::engine::general_purpose::STANDARD.encode(content.as_bytes());
    let mut body = serde_json::json!({
        "message": message,
        "content": encoded,
        "branch": branch,
    });
    if let Some(s) = sha {
        body["sha"] = serde_json::Value::String(s.to_string());
    }
    let json = client.request("PUT", &url, Some(body))?;
    json.get("content")
        .and_then(|c| c.get("sha"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .ok_or_else(|| GitHubError::Parse("put response missing content.sha".to_string()))
}

/// Makes sure `branch` exists, creating it as an orphan branch (a root
/// commit over an empty tree, no parent) if not — so it starts out
/// containing nothing rather than a copy of the whole repo, since `put_file`
/// never removes what a branch inherited from wherever it was cut from. The
/// Contents API commits onto an existing branch ref only — it does not
/// create one implicitly.
pub fn ensure_branch(client: &Client, repo: &RepoId, branch: &str) -> Result<(), GitHubError> {
    let ref_path = format!(
        "/repos/{}/{}/git/ref/heads/{}",
        repo.owner,
        repo.repo,
        crate::github::encode_query(branch)
    );
    match client.request("GET", &ref_path, None) {
        Ok(_) => Ok(()),
        Err(GitHubError::NotFound) => {
            let tree_path = format!("/repos/{}/{}/git/trees", repo.owner, repo.repo);
            let tree =
                client.request("POST", &tree_path, Some(serde_json::json!({ "tree": [] })))?;
            let tree_sha = tree
                .get("sha")
                .and_then(|s| s.as_str())
                .ok_or_else(|| GitHubError::Parse("tree response missing sha".to_string()))?;

            let commit_path = format!("/repos/{}/{}/git/commits", repo.owner, repo.repo);
            let commit = client.request(
                "POST",
                &commit_path,
                Some(serde_json::json!({
                    "message": format!("chore: init {branch} (orphan)"),
                    "tree": tree_sha,
                    "parents": Vec::<String>::new(),
                })),
            )?;
            let commit_sha = commit
                .get("sha")
                .and_then(|s| s.as_str())
                .ok_or_else(|| GitHubError::Parse("commit response missing sha".to_string()))?;

            let create_path = format!("/repos/{}/{}/git/refs", repo.owner, repo.repo);
            let create_result = client.request(
                "POST",
                &create_path,
                Some(serde_json::json!({
                    "ref": format!("refs/heads/{branch}"),
                    "sha": commit_sha,
                })),
            );
            // 422 here almost always means another sync run won the race and
            // created the branch between our GET and this POST -- confirm
            // that before treating it as a real failure.
            if let Err(GitHubError::Http { status: 422, .. }) = &create_result
                && client.request("GET", &ref_path, None).is_ok()
            {
                return Ok(());
            }
            create_result.map(|_| ())
        }
        Err(e) => Err(e),
    }
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
    fn get_file_decodes_wrapped_base64() {
        // "hello\nworld\n" base64-encoded, split across lines the way GitHub
        // actually wraps it, to prove the whitespace-strip step is load-bearing.
        let b64 = base64::engine::general_purpose::STANDARD.encode("hello\nworld\n");
        let (first, second) = b64.split_at(b64.len() / 2);
        let server = MockServer::start(vec![MockResponse::json(
            200,
            &format!(r#"{{"sha":"abc123","content":"{first}\n{second}\n","encoding":"base64"}}"#),
        )]);
        let client = server.client(None);
        let file = get_file(&client, &repo(), "memory-sync.jsonl", "agentflare-memory")
            .unwrap()
            .unwrap();
        assert_eq!(file.content, "hello\nworld\n");
        assert_eq!(file.sha, "abc123");
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/contents/memory-sync.jsonl?ref=agentflare-memory"
        );
    }

    #[test]
    fn get_file_returns_none_on_404() {
        let server = MockServer::start(vec![MockResponse::json(404, r#"{"message":"Not Found"}"#)]);
        let client = server.client(None);
        assert!(
            get_file(&client, &repo(), "x.jsonl", "main")
                .unwrap()
                .is_none()
        );
        let _ = server.requests();
    }

    #[test]
    fn put_file_encodes_body_and_returns_new_sha() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"content":{"sha":"def456"}}"#,
        )]);
        let client = server.client(Some("tok"));
        let new_sha = put_file(
            &client,
            &repo(),
            "memory-sync.jsonl",
            "agentflare-memory",
            "chore: sync",
            "line1\nline2\n",
            Some("abc123"),
        )
        .unwrap();
        assert_eq!(new_sha, "def456");

        let reqs = server.requests();
        assert_eq!(reqs[0].method, "PUT");
        assert_eq!(reqs[0].path, "/repos/o/r/contents/memory-sync.jsonl");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["sha"], "abc123");
        assert_eq!(sent["branch"], "agentflare-memory");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(sent["content"].as_str().unwrap())
            .unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "line1\nline2\n");
    }

    #[test]
    fn ensure_branch_is_a_noop_when_the_branch_already_exists() {
        let server = MockServer::start(vec![MockResponse::json(200, r#"{"object":{"sha":"x"}}"#)]);
        let client = server.client(None);
        ensure_branch(&client, &repo(), "agentflare-memory").unwrap();
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, "/repos/o/r/git/ref/heads/agentflare-memory");
    }

    #[test]
    fn ensure_branch_creates_an_orphan_commit_when_missing() {
        let server = MockServer::start(vec![
            MockResponse::json(404, r#"{"message":"Not Found"}"#),
            MockResponse::json(201, r#"{"sha":"emptytree"}"#),
            MockResponse::json(201, r#"{"sha":"orphancommit"}"#),
            MockResponse::json(201, r#"{"ref":"refs/heads/agentflare-memory"}"#),
        ]);
        let client = server.client(Some("tok"));
        ensure_branch(&client, &repo(), "agentflare-memory").unwrap();

        let reqs = server.requests();
        assert_eq!(reqs[0].path, "/repos/o/r/git/ref/heads/agentflare-memory");

        assert_eq!(reqs[1].method, "POST");
        assert_eq!(reqs[1].path, "/repos/o/r/git/trees");
        let tree_sent: serde_json::Value = serde_json::from_str(&reqs[1].body).unwrap();
        assert_eq!(tree_sent["tree"], serde_json::json!([]));

        assert_eq!(reqs[2].method, "POST");
        assert_eq!(reqs[2].path, "/repos/o/r/git/commits");
        let commit_sent: serde_json::Value = serde_json::from_str(&reqs[2].body).unwrap();
        assert_eq!(commit_sent["tree"], "emptytree");
        assert_eq!(commit_sent["parents"], serde_json::json!([]));

        assert_eq!(reqs[3].method, "POST");
        assert_eq!(reqs[3].path, "/repos/o/r/git/refs");
        let sent: serde_json::Value = serde_json::from_str(&reqs[3].body).unwrap();
        assert_eq!(sent["ref"], "refs/heads/agentflare-memory");
        assert_eq!(sent["sha"], "orphancommit");
    }

    #[test]
    fn get_file_rejects_a_non_base64_encoding() {
        // Files over the Contents API's 1MB inline limit come back this way
        // instead of erroring -- must not be read as an empty file.
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"sha":"abc123","content":"","encoding":"none"}"#,
        )]);
        let client = server.client(None);
        let err = get_file(&client, &repo(), "memory-sync.jsonl", "agentflare-memory").unwrap_err();
        assert!(
            matches!(err, GitHubError::Parse(_)),
            "expected Parse, got {err:?}"
        );
    }

    #[test]
    fn ensure_branch_treats_a_concurrent_create_422_as_success_when_the_branch_now_exists() {
        let server = MockServer::start(vec![
            MockResponse::json(404, r#"{"message":"Not Found"}"#),
            MockResponse::json(201, r#"{"sha":"emptytree"}"#),
            MockResponse::json(201, r#"{"sha":"orphancommit"}"#),
            MockResponse::json(422, r#"{"message":"Reference already exists"}"#),
            MockResponse::json(200, r#"{"object":{"sha":"tip123"}}"#),
        ]);
        let client = server.client(Some("tok"));
        ensure_branch(&client, &repo(), "agentflare-memory").unwrap();

        let reqs = server.requests();
        assert_eq!(reqs.len(), 5);
        assert_eq!(reqs[3].method, "POST");
        assert_eq!(reqs[4].method, "GET");
        assert_eq!(reqs[4].path, "/repos/o/r/git/ref/heads/agentflare-memory");
    }

    #[test]
    fn ensure_branch_propagates_a_422_when_the_branch_still_does_not_exist() {
        let server = MockServer::start(vec![
            MockResponse::json(404, r#"{"message":"Not Found"}"#),
            MockResponse::json(201, r#"{"sha":"emptytree"}"#),
            MockResponse::json(201, r#"{"sha":"orphancommit"}"#),
            MockResponse::json(422, r#"{"message":"Validation Failed"}"#),
            MockResponse::json(404, r#"{"message":"Not Found"}"#),
        ]);
        let client = server.client(Some("tok"));
        let err = ensure_branch(&client, &repo(), "agentflare-memory").unwrap_err();
        assert!(
            matches!(err, GitHubError::Http { status: 422, .. }),
            "expected the original 422, got {err:?}"
        );
    }
}
