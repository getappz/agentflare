//! GitHub Actions operations. `list_runs` unwraps the `{workflow_runs: [...]}`
//! envelope; `rerun`/`dispatch` ignore their empty response bodies.

use crate::github::models::WorkflowRun;
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
}
