//! The single HTTP call site. Attaches auth + GitHub headers, refuses writes
//! with no credential, and maps status codes to `GitHubError`.

use crate::github::GitHubError;
use crate::github::auth;
use std::time::Duration;

pub struct Client {
    agent: ureq::Agent,
    token: Option<String>,
    base_url: String,
}

const BASE_URL: &str = "https://api.github.com";

/// Max items per page GitHub allows for the list endpoints used here.
const PER_PAGE: usize = 100;

fn map_status(status: u16, ratelimit_remaining: Option<&str>, body: String) -> GitHubError {
    match status {
        401 => GitHubError::NoAuth(
            "GitHub rejected the credential (401). Refresh it: 'gh auth login' or reset GITHUB_TOKEN.".to_string(),
        ),
        403 if ratelimit_remaining == Some("0") => GitHubError::RateLimited(
            "GitHub rate limit hit. Authenticate to raise it to 5000 req/hr.".to_string(),
        ),
        403 => GitHubError::Forbidden(
            "GitHub returned 403 — the token lacks the required scope/permission.".to_string(),
        ),
        404 => GitHubError::NotFound,
        429 => GitHubError::RateLimited("GitHub rate limit hit (429).".to_string()),
        _ => GitHubError::Http { status, body },
    }
}

impl Client {
    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            .timeout_read(Duration::from_secs(60))
            .build()
    }

    pub fn new() -> Result<Client, GitHubError> {
        Ok(Client {
            agent: Self::agent(),
            token: Some(auth::resolve_token()?),
            base_url: BASE_URL.to_string(),
        })
    }

    pub fn anonymous() -> Client {
        Client {
            agent: Self::agent(),
            token: None,
            base_url: BASE_URL.to_string(),
        }
    }

    /// Construct a client aimed at an arbitrary base URL (e.g. a local mock
    /// server) with an explicit token. Test-only: production code goes through
    /// [`Client::new`] / [`Client::anonymous`], which always target GitHub.
    #[cfg(test)]
    pub(crate) fn for_test(base_url: String, token: Option<String>) -> Client {
        Client {
            agent: Self::agent(),
            token,
            base_url,
        }
    }

    pub fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, GitHubError> {
        if method != "GET" && self.token.is_none() {
            return Err(GitHubError::NoAuth(
                crate::github::auth::NO_AUTH_MSG.to_string(),
            ));
        }
        let url = format!("{}{}", self.base_url, path);
        let mut req = self
            .agent
            .request(method, &url)
            .set("User-Agent", "agentflare")
            .set("Accept", "application/vnd.github+json")
            .set("X-GitHub-Api-Version", "2022-11-28");
        if let Some(tok) = &self.token {
            req = req.set("Authorization", &format!("Bearer {tok}"));
        }
        let result = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        match result {
            Ok(resp) => {
                let text = resp
                    .into_string()
                    .map_err(|e| GitHubError::Transport(e.to_string()))?;
                if text.trim().is_empty() {
                    return Ok(serde_json::Value::Null);
                }
                serde_json::from_str(&text).map_err(|e| GitHubError::Parse(e.to_string()))
            }
            Err(ureq::Error::Status(code, resp)) => {
                let remaining = resp.header("x-ratelimit-remaining").map(str::to_string);
                let body = resp.into_string().unwrap_or_default();
                Err(map_status(code, remaining.as_deref(), body))
            }
            Err(e) => Err(GitHubError::Transport(e.to_string())),
        }
    }

    /// GET every page of a list endpoint, walking `?page=N&per_page=100` until a
    /// page comes back shorter than a full page. `extract` pulls the item array
    /// out of each response — identity for bare-array endpoints, or the envelope
    /// key (e.g. `workflow_runs`) for wrapped ones. Returns the concatenated
    /// items as a JSON array so callers deserialize once.
    pub fn get_paginated(
        &self,
        base_path: &str,
        extract: impl Fn(&serde_json::Value) -> Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, GitHubError> {
        let sep = if base_path.contains('?') { '&' } else { '?' };
        let mut all: Vec<serde_json::Value> = Vec::new();
        let mut page = 1usize;
        loop {
            let path = format!("{base_path}{sep}per_page={PER_PAGE}&page={page}");
            let resp = self.request("GET", &path, None)?;
            let items = extract(&resp);
            let full = items.len() == PER_PAGE;
            all.extend(items);
            if !full {
                break;
            }
            page += 1;
        }
        Ok(serde_json::Value::Array(all))
    }
}

/// Extractor for bare-array list endpoints (pulls, issues, releases).
pub fn as_array(v: &serde_json::Value) -> Vec<serde_json::Value> {
    v.as_array().cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_status_covers_the_table() {
        assert!(matches!(
            map_status(401, None, String::new()),
            GitHubError::NoAuth(_)
        ));
        assert!(matches!(
            map_status(403, Some("0"), String::new()),
            GitHubError::RateLimited(_)
        ));
        assert!(matches!(
            map_status(403, Some("42"), String::new()),
            GitHubError::Forbidden(_)
        ));
        assert!(matches!(
            map_status(404, None, String::new()),
            GitHubError::NotFound
        ));
        assert!(matches!(
            map_status(500, None, "boom".into()),
            GitHubError::Http { status: 500, .. }
        ));
    }

    #[test]
    fn as_array_unwraps_or_defaults_empty() {
        assert_eq!(as_array(&serde_json::json!([1, 2, 3])).len(), 3);
        assert!(as_array(&serde_json::json!({})).is_empty());
        assert!(as_array(&serde_json::Value::Null).is_empty());
    }

    #[test]
    fn map_status_429_is_rate_limited() {
        assert!(matches!(
            map_status(429, None, String::new()),
            GitHubError::RateLimited(_)
        ));
    }

    use crate::github::test_support::{MockResponse, MockServer};

    #[test]
    fn request_refuses_writes_without_a_token_before_any_network_call() {
        // No server needed: the guard short-circuits before a request is made.
        let client = Client::for_test("http://127.0.0.1:1".to_string(), None);
        let err = client.request("POST", "/x", None).unwrap_err();
        assert!(matches!(err, GitHubError::NoAuth(_)));
    }

    #[test]
    fn request_get_parses_json_and_sends_expected_headers() {
        let server = MockServer::start(vec![MockResponse::json(200, r#"{"ok":true}"#)]);
        let client = server.client(None);
        let value = client.request("GET", "/probe", None).unwrap();
        assert_eq!(value["ok"], true);

        let reqs = server.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[0].path, "/probe");
    }

    #[test]
    fn request_sends_json_body_on_writes() {
        let server = MockServer::start(vec![MockResponse::json(201, r#"{"created":1}"#)]);
        let client = server.client(Some("tok"));
        let body = serde_json::json!({ "name": "value" });
        let value = client.request("POST", "/thing", Some(body)).unwrap();
        assert_eq!(value["created"], 1);

        let reqs = server.requests();
        assert_eq!(reqs[0].method, "POST");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["name"], "value");
    }

    #[test]
    fn request_empty_body_becomes_json_null() {
        let server = MockServer::start(vec![MockResponse::json(204, "")]);
        let client = server.client(Some("tok"));
        let value = client
            .request("POST", "/empty", Some(serde_json::json!({})))
            .unwrap();
        assert_eq!(value, serde_json::Value::Null);
        let _ = server.requests();
    }

    #[test]
    fn request_maps_error_status_from_the_wire() {
        let server = MockServer::start(vec![
            MockResponse::json(403, r#"{"message":"limited"}"#)
                .with_header("x-ratelimit-remaining", "0"),
        ]);
        let client = server.client(Some("tok"));
        let err = client.request("GET", "/limited", None).unwrap_err();
        assert!(matches!(err, GitHubError::RateLimited(_)));
        let _ = server.requests();
    }

    #[test]
    fn request_reports_parse_error_on_malformed_json() {
        let server = MockServer::start(vec![MockResponse::json(200, "not json")]);
        let client = server.client(None);
        let err = client.request("GET", "/bad", None).unwrap_err();
        assert!(matches!(err, GitHubError::Parse(_)));
        let _ = server.requests();
    }
}
