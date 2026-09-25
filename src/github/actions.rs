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

/// Legacy commit statuses (the Statuses API -- third-party CI, CLA bots,
/// deploy previews) for a commit SHA, folded into the check-run shape via
/// `CheckRun::from_status`. The Checks API behind `list_check_runs` never
/// reports these, yet branch protection can require them, so a CI verdict
/// built from check runs alone could call a PR green while a required
/// status was still pending or red. One page of 100 covers every realistic
/// commit; the combined endpoint already de-duplicates to each context's
/// latest state.
/// Posts a Statuses-API status on commit `sha` under `context`. A status is
/// per commit, so a context branch protection requires becomes a merge-time
/// gate on exactly the heads that carry it: `supervisor::merge_approved_pr`
/// marks the head it judged (approval label on, no unresolved CodeRabbit
/// findings) so a repo that requires the context lets GitHub's auto-merge
/// land only judged heads, and a later push -- a new sha without it -- waits
/// for the sweep to judge it. `state` is `success`, `pending`, `failure` or
/// `error`.
pub fn create_commit_status(
    client: &Client,
    repo: &RepoId,
    sha: &str,
    state: &str,
    context: &str,
    description: &str,
) -> Result<(), GitHubError> {
    let path = format!("/repos/{}/{}/statuses/{sha}", repo.owner, repo.repo);
    client.request(
        "POST",
        &path,
        Some(serde_json::json!({
            "state": state,
            "context": context,
            "description": description,
        })),
    )?;
    Ok(())
}

pub fn list_commit_statuses(
    client: &Client,
    repo: &RepoId,
    sha: &str,
) -> Result<Vec<CheckRun>, GitHubError> {
    let path = format!(
        "/repos/{}/{}/commits/{sha}/status?per_page=100",
        repo.owner, repo.repo
    );
    let json = client.request("GET", &path, None)?;
    Ok(json["statuses"]
        .as_array()
        .map(|statuses| {
            statuses
                .iter()
                .filter_map(|st| {
                    Some(CheckRun::from_status(
                        st["context"].as_str()?,
                        st["state"].as_str().unwrap_or_default(),
                        false,
                    ))
                })
                .collect()
        })
        .unwrap_or_default())
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

    #[test]
    fn create_commit_status_posts_state_context_and_description() {
        let server = MockServer::start(vec![MockResponse::json(201, r#"{"id":1}"#)]);
        let client = server.client(Some("tok"));
        create_commit_status(
            &client,
            &repo(),
            "abc123",
            "success",
            "agentflare/judged",
            "approval label on, no unresolved findings",
        )
        .unwrap();
        let reqs = server.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/repos/o/r/statuses/abc123");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["state"], "success");
        assert_eq!(sent["context"], "agentflare/judged");
        assert_eq!(
            sent["description"],
            "approval label on, no unresolved findings"
        );
    }

    #[test]
    fn list_commit_statuses_folds_legacy_statuses_into_check_runs() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"state":"pending","statuses":[
                {"context":"ci/jenkins","state":"success"},
                {"context":"cla","state":"pending"},
                {"context":"lint","state":"error"}
            ]}"#,
        )]);
        let client = server.client(None);
        let runs = list_commit_statuses(&client, &repo(), "abc123").unwrap();
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].conclusion.as_deref(), Some("success"));
        assert_eq!(runs[1].status, "pending");
        assert_eq!(runs[1].conclusion, None);
        assert_eq!(runs[2].conclusion.as_deref(), Some("failure"));
        assert_eq!(
            server.requests()[0].path,
            "/repos/o/r/commits/abc123/status?per_page=100"
        );
    }
}
