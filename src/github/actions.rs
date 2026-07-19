//! GitHub Actions operations. `list_runs` unwraps the `{workflow_runs: [...]}`
//! envelope; `rerun`/`dispatch` ignore their empty response bodies.

use crate::github::models::{CheckRun, WorkflowRun};
use crate::github::{Client, GitHubError, RepoId};

/// Extractor for the `{ workflow_runs: [...] }` envelope each page returns.
fn workflow_runs(page: &serde_json::Value) -> Vec<serde_json::Value> {
    page.get("workflow_runs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

fn dispatch_body(git_ref: &str, inputs: Option<&serde_json::Value>) -> serde_json::Value {
    let mut v = serde_json::json!({ "ref": git_ref });
    if let Some(i) = inputs {
        v["inputs"] = i.clone();
    }
    v
}

pub fn list_runs(
    client: &Client,
    repo: &RepoId,
    branch: Option<&str>,
) -> Result<Vec<WorkflowRun>, GitHubError> {
    let mut path = format!("/repos/{}/{}/actions/runs", repo.owner, repo.repo);
    if let Some(b) = branch {
        path.push_str(&format!("?branch={}", crate::github::encode_query(b)));
    }
    let arr = client.get_paginated(&path, workflow_runs)?;
    serde_json::from_value(arr).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn get_run(client: &Client, repo: &RepoId, run_id: u64) -> Result<WorkflowRun, GitHubError> {
    let path = format!("/repos/{}/{}/actions/runs/{run_id}", repo.owner, repo.repo);
    let json = client.request("GET", &path, None)?;
    serde_json::from_value(json).map_err(|e| GitHubError::Parse(e.to_string()))
}

pub fn rerun(client: &Client, repo: &RepoId, run_id: u64) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/actions/runs/{run_id}/rerun",
        repo.owner, repo.repo
    );
    client.request("POST", &path, Some(serde_json::json!({})))?;
    Ok(())
}

/// `workflow` is a workflow file name (e.g. "ci.yml") or numeric id.
pub fn dispatch(
    client: &Client,
    repo: &RepoId,
    workflow: &str,
    git_ref: &str,
    inputs: Option<&serde_json::Value>,
) -> Result<(), GitHubError> {
    let path = format!(
        "/repos/{}/{}/actions/workflows/{workflow}/dispatches",
        repo.owner, repo.repo
    );
    client.request("POST", &path, Some(dispatch_body(git_ref, inputs)))?;
    Ok(())
}

/// Extractor for the `{ check_runs: [...] }` envelope each page returns.
fn check_runs(page: &serde_json::Value) -> Vec<serde_json::Value> {
    page.get("check_runs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// CI check runs (status checks) for a commit SHA — used by `pr_status` to
/// report build/lint/test state without a separate `gh pr checks` round trip.
pub fn list_check_runs(
    client: &Client,
    repo: &RepoId,
    sha: &str,
) -> Result<Vec<CheckRun>, GitHubError> {
    let path = format!(
        "/repos/{}/{}/commits/{sha}/check-runs",
        repo.owner, repo.repo
    );
    let arr = client.get_paginated(&path, check_runs)?;
    serde_json::from_value(arr).map_err(|e| GitHubError::Parse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workflow_runs_extracts_the_array() {
        let env = serde_json::json!({ "total_count": 1, "workflow_runs": [{
            "id": 1, "status": "completed", "conclusion": "success",
            "html_url": "https://github.com/o/r/actions/runs/1" }] });
        let items = workflow_runs(&env);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["conclusion"], "success");
    }
    #[test]
    fn workflow_runs_defaults_to_empty_when_key_absent() {
        assert!(workflow_runs(&serde_json::json!({})).is_empty());
    }
    #[test]
    fn dispatch_body_includes_inputs_only_when_present() {
        let with = dispatch_body("main", Some(&serde_json::json!({"env": "prod"})));
        assert_eq!(with["ref"], "main");
        assert_eq!(with["inputs"]["env"], "prod");
        assert!(dispatch_body("main", None).get("inputs").is_none());
    }

    use crate::github::test_support::{MockResponse, MockServer};

    fn repo() -> RepoId {
        RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    #[test]
    fn list_runs_unwraps_envelope_without_branch() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"workflow_runs":[{"id":1,"status":"completed","conclusion":"success","html_url":"u"}]}"#,
        )]);
        let client = server.client(None);
        let runs = list_runs(&client, &repo(), None).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/actions/runs?per_page=100&page=1"
        );
    }

    #[test]
    fn list_runs_appends_branch_query() {
        let server = MockServer::start(vec![MockResponse::json(200, r#"{"workflow_runs":[]}"#)]);
        let client = server.client(None);
        list_runs(&client, &repo(), Some("feat/x")).unwrap();
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/actions/runs?branch=feat/x&per_page=100&page=1"
        );
    }

    #[test]
    fn get_run_fetches_single_run() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"id":7,"status":"completed","conclusion":"failure","html_url":"u"}"#,
        )]);
        let client = server.client(None);
        let run = get_run(&client, &repo(), 7).unwrap();
        assert_eq!(run.conclusion.as_deref(), Some("failure"));
        assert_eq!(server.requests()[0].path, "/repos/o/r/actions/runs/7");
    }

    #[test]
    fn rerun_posts_to_the_rerun_endpoint() {
        let server = MockServer::start(vec![MockResponse::json(201, "")]);
        let client = server.client(Some("tok"));
        rerun(&client, &repo(), 9).unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/repos/o/r/actions/runs/9/rerun");
    }

    #[test]
    fn dispatch_posts_ref_to_workflow_dispatches() {
        let server = MockServer::start(vec![MockResponse::json(204, "")]);
        let client = server.client(Some("tok"));
        dispatch(&client, &repo(), "ci.yml", "main", None).unwrap();
        let reqs = server.requests();
        assert_eq!(
            reqs[0].path,
            "/repos/o/r/actions/workflows/ci.yml/dispatches"
        );
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["ref"], "main");
    }

    #[test]
    fn check_runs_extracts_the_array() {
        let env = serde_json::json!({ "total_count": 1, "check_runs": [{"name": "build", "status": "completed", "conclusion": "success"}] });
        let items = check_runs(&env);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["name"], "build");
    }

    #[test]
    fn list_check_runs_fetches_and_unwraps_envelope() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"check_runs":[{"name":"build","status":"completed","conclusion":"success"}]}"#,
        )]);
        let client = server.client(None);
        let runs = list_check_runs(&client, &repo(), "abc123").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].name, "build");
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/commits/abc123/check-runs?per_page=100&page=1"
        );
    }
}
