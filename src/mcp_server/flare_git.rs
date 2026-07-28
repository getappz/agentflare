//! `flare_git` MCP tool handler body -- split out of mcp_server.rs (item #168).

use super::*;

impl AgentflareMcp {
    pub fn flare_git_impl(&self, req: GitHubRequest) -> Result<String, ErrorData> {
        use crate::github::{Client, RepoId, actions, issues, mcp, pulls, releases, repos};

        // Keep in sync with the action names matched below — validating
        // first means an unknown action fails before repo/client setup
        // (and credentials) rather than surfacing as an unrelated error.
        const KNOWN_ACTIONS: &[&str] = &[
            "pr_create",
            "pr_list",
            "pr_get",
            "pr_status",
            "pr_merge",
            "pr_comment",
            "pr_request_review",
            "issue_create",
            "issue_list",
            "issue_get",
            "issue_comment",
            "issue_close",
            "issue_label",
            "release_list",
            "release_get",
            "release_latest",
            "release_create",
            "run_list",
            "run_get",
            "run_rerun",
            "workflow_dispatch",
        ];
        if !KNOWN_ACTIONS.contains(&req.action.as_str()) {
            return Err(ErrorData::invalid_params(
                format!("unknown action: {}", req.action),
                None,
            ));
        }

        let repo = match &req.repo {
            Some(r) => RepoId::parse(r)
                .ok_or_else(|| ErrorData::invalid_params(format!("bad repo: {r}"), None))?,
            None => RepoId::resolve_from_remote(&std::env::current_dir().unwrap_or_default())
                .ok_or_else(|| {
                    ErrorData::invalid_params(
                        "no repo given and could not resolve origin remote".to_string(),
                        None,
                    )
                })?,
        };
        let client = Client::new().map_err(to_mcp_error)?;

        let out = match req.action.as_str() {
            "pr_create" => {
                let title = req
                    .title
                    .as_deref()
                    .ok_or_else(|| ErrorData::invalid_params("title is required", None))?;
                Self::validate_conventional_pr_title(title)
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let head = req
                    .head
                    .as_deref()
                    .ok_or_else(|| ErrorData::invalid_params("head is required", None))?;
                let base = req
                    .base
                    .as_deref()
                    .ok_or_else(|| ErrorData::invalid_params("base is required", None))?;
                let pr = pulls::create(&client, &repo, title, head, base, req.body.as_deref())
                    .map_err(to_mcp_error)?;
                format!("Opened PR #{}: {}", pr.number, pr.html_url)
            }
            "pr_list" => {
                let state = req.state.as_deref().unwrap_or("open");
                let prs = pulls::list(&client, &repo, state).map_err(to_mcp_error)?;
                serde_json::to_string(&prs.iter().map(|p| &p.html_url).collect::<Vec<_>>())
                    .unwrap_or_default()
            }
            "pr_get" => {
                let n = req
                    .number
                    .ok_or_else(|| ErrorData::invalid_params("number is required", None))?;
                let pr = pulls::get(&client, &repo, n).map_err(to_mcp_error)?;
                format!(
                    "PR #{} [{}] {}: {}",
                    pr.number, pr.state, pr.title, pr.html_url
                )
            }
            "pr_status" => {
                let n = req
                    .number
                    .ok_or_else(|| ErrorData::invalid_params("number is required", None))?;
                let pr = pulls::get(&client, &repo, n).map_err(to_mcp_error)?;
                let sha = pr.head.as_ref().map(|h| h.sha.as_str()).unwrap_or_default();
                let since = req.since.as_deref();
                let checks = actions::list_check_runs(&client, &repo, sha).map_err(to_mcp_error)?;
                let reviews = pulls::list_reviews(&client, &repo, n).map_err(to_mcp_error)?;
                let review_comments =
                    pulls::list_review_comments(&client, &repo, n, since).map_err(to_mcp_error)?;
                let resolved_ids =
                    pulls::resolved_review_comment_ids(&client, &repo, n).map_err(to_mcp_error)?;
                let comments =
                    issues::list_comments(&client, &repo, n, since).map_err(to_mcp_error)?;
                mcp::pr_status_json(
                    &pr,
                    &checks,
                    &reviews,
                    &review_comments,
                    &resolved_ids,
                    &comments,
                )
            }
            "pr_merge" => {
                let n = req
                    .number
                    .ok_or_else(|| ErrorData::invalid_params("number is required", None))?;
                let method = req.merge_method.as_deref().unwrap_or("merge");
                pulls::merge(&client, &repo, n, method).map_err(to_mcp_error)?;
                format!("Merged PR #{n} ({method})")
            }
            "pr_comment" => {
                let n = req
                    .number
                    .ok_or_else(|| ErrorData::invalid_params("number is required", None))?;
                let body = req
                    .body
                    .as_deref()
                    .ok_or_else(|| ErrorData::invalid_params("body is required", None))?;
                pulls::comment(&client, &repo, n, body).map_err(to_mcp_error)?;
                format!("Commented on PR #{n}")
            }
            "pr_request_review" => {
                let n = req
                    .number
                    .ok_or_else(|| ErrorData::invalid_params("number is required", None))?;
                let reviewers = req.reviewers.clone().unwrap_or_default();
                pulls::request_review(&client, &repo, n, &reviewers).map_err(to_mcp_error)?;
                format!("Requested review on PR #{n}")
            }
            "issue_create" => {
                let title = req
                    .title
                    .as_deref()
                    .ok_or_else(|| ErrorData::invalid_params("title is required", None))?;
                let labels = req.labels.clone().unwrap_or_default();
                let assignees = req.assignees.clone().unwrap_or_default();
                let issue = issues::create(
                    &client,
                    &repo,
                    title,
                    req.body.as_deref(),
                    &labels,
                    &assignees,
                )
                .map_err(to_mcp_error)?;
                format!("Opened issue #{}: {}", issue.number, issue.html_url)
            }
            "issue_list" => {
                let state = req.state.as_deref().unwrap_or("open");
                let items = issues::list(&client, &repo, state).map_err(to_mcp_error)?;
                serde_json::to_string(&items.iter().map(|i| &i.html_url).collect::<Vec<_>>())
                    .unwrap_or_default()
            }
            "issue_get" => {
                let n = req
                    .number
                    .ok_or_else(|| ErrorData::invalid_params("number is required", None))?;
                let issue = issues::get(&client, &repo, n).map_err(to_mcp_error)?;
                format!(
                    "Issue #{} [{}] {}: {}",
                    issue.number, issue.state, issue.title, issue.html_url
                )
            }
            "issue_comment" => {
                let n = req
                    .number
                    .ok_or_else(|| ErrorData::invalid_params("number is required", None))?;
                let body = req
                    .body
                    .as_deref()
                    .ok_or_else(|| ErrorData::invalid_params("body is required", None))?;
                issues::comment(&client, &repo, n, body).map_err(to_mcp_error)?;
                format!("Commented on issue #{n}")
            }
            "issue_close" => {
                let n = req
                    .number
                    .ok_or_else(|| ErrorData::invalid_params("number is required", None))?;
                let issue = issues::close(&client, &repo, n).map_err(to_mcp_error)?;
                format!("Closed issue #{} [{}]", issue.number, issue.state)
            }
            "issue_label" => {
                let n = req
                    .number
                    .ok_or_else(|| ErrorData::invalid_params("number is required", None))?;
                let labels = req.labels.clone().unwrap_or_default();
                issues::add_labels(&client, &repo, n, &labels).map_err(to_mcp_error)?;
                format!("Added {} label(s) to issue #{n}", labels.len())
            }
            "release_list" => {
                let rels = releases::list(&client, &repo).map_err(to_mcp_error)?;
                serde_json::to_string(&rels.iter().map(|r| &r.tag_name).collect::<Vec<_>>())
                    .unwrap_or_default()
            }
            "release_get" => {
                let id = req
                    .release_id
                    .ok_or_else(|| ErrorData::invalid_params("release_id is required", None))?;
                let rel = releases::get(&client, &repo, id).map_err(to_mcp_error)?;
                format!(
                    "Release {} [{}]: {}",
                    rel.tag_name,
                    if rel.prerelease { "pre" } else { "stable" },
                    rel.html_url
                )
            }
            "release_latest" => {
                let rel = releases::latest(&client, &repo).map_err(to_mcp_error)?;
                format!("Latest: {} — {}", rel.tag_name, rel.html_url)
            }
            "release_create" => {
                let tag = req
                    .tag
                    .as_deref()
                    .ok_or_else(|| ErrorData::invalid_params("tag is required", None))?;
                let rel = releases::create(
                    &client,
                    &repo,
                    tag,
                    req.name.as_deref(),
                    req.body.as_deref(),
                    req.draft.unwrap_or(false),
                    req.prerelease.unwrap_or(false),
                )
                .map_err(to_mcp_error)?;
                format!("Created release {}: {}", rel.tag_name, rel.html_url)
            }
            "run_list" => {
                let runs = actions::list_runs(&client, &repo, req.branch.as_deref())
                    .map_err(to_mcp_error)?;
                let summary: Vec<String> = runs
                    .iter()
                    .map(|r| {
                        format!(
                            "{} {} {}",
                            r.id,
                            r.status,
                            r.conclusion.as_deref().unwrap_or("-")
                        )
                    })
                    .collect();
                serde_json::to_string(&summary).unwrap_or_default()
            }
            "run_get" => {
                let id = req
                    .run_id
                    .ok_or_else(|| ErrorData::invalid_params("run_id is required", None))?;
                let run = actions::get_run(&client, &repo, id).map_err(to_mcp_error)?;
                format!(
                    "Run {} [{}/{}]: {}",
                    run.id,
                    run.status,
                    run.conclusion.as_deref().unwrap_or("-"),
                    run.html_url
                )
            }
            "run_rerun" => {
                let id = req
                    .run_id
                    .ok_or_else(|| ErrorData::invalid_params("run_id is required", None))?;
                actions::rerun(&client, &repo, id).map_err(to_mcp_error)?;
                format!("Re-queued run {id}")
            }
            "workflow_dispatch" => {
                let wf = req
                    .workflow
                    .as_deref()
                    .ok_or_else(|| ErrorData::invalid_params("workflow is required", None))?;
                if req.inputs.as_ref().is_some_and(|v| !v.is_object()) {
                    return Err(ErrorData::invalid_params(
                        "inputs must be a JSON object",
                        None,
                    ));
                }
                let git_ref = resolve_workflow_git_ref(
                    req.git_ref.as_deref(),
                    req.repo.is_some(),
                    || repos::get_default_branch(&client, &repo),
                    || {
                        flare_git_core::branch::resolve_default_branch(
                            &std::env::current_dir().unwrap_or_default(),
                        )
                    },
                )
                .map_err(to_mcp_error)?;
                actions::dispatch(&client, &repo, wf, &git_ref, req.inputs.as_ref())
                    .map_err(to_mcp_error)?;
                format!("Dispatched {wf} on {git_ref}")
            }
            other => {
                return Err(ErrorData::invalid_params(
                    format!("unknown action: {other}"),
                    None,
                ));
            }
        };
        Ok(out)
    }
}

/// Decides the git ref for `workflow_dispatch`: an explicit `git_ref` wins;
/// otherwise an overridden `repo` resolves its default branch via
/// `remote_default` (a GitHub API call), and the no-override case via
/// `local_default` (the checkout's origin). Pulled out standalone so both
/// paths are unit-testable without a network-backed `Client`.
fn resolve_workflow_git_ref(
    explicit: Option<&str>,
    repo_overridden: bool,
    remote_default: impl FnOnce() -> Result<String, crate::github::GitHubError>,
    local_default: impl FnOnce() -> String,
) -> Result<String, crate::github::GitHubError> {
    match explicit {
        Some(r) => Ok(r.to_string()),
        None if repo_overridden => remote_default(),
        None => Ok(local_default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_workflow_git_ref_prefers_the_explicit_ref() {
        let r = resolve_workflow_git_ref(
            Some("refs/heads/explicit"),
            true,
            || panic!("must not resolve a default when a ref is given"),
            || panic!("must not resolve a default when a ref is given"),
        );
        assert_eq!(r.unwrap(), "refs/heads/explicit");
    }

    #[test]
    fn resolve_workflow_git_ref_uses_the_remote_api_when_repo_is_overridden() {
        let r = resolve_workflow_git_ref(
            None,
            true,
            || Ok("main".to_string()),
            || panic!("an overridden repo must resolve via the API, not local git"),
        );
        assert_eq!(r.unwrap(), "main");
    }

    #[test]
    fn resolve_workflow_git_ref_uses_local_resolution_without_a_repo_override() {
        let r = resolve_workflow_git_ref(
            None,
            false,
            || panic!("no repo override must not hit the GitHub API"),
            || "develop".to_string(),
        );
        assert_eq!(r.unwrap(), "develop");
    }

    #[test]
    fn unknown_action_is_rejected_before_repo_or_client_setup() {
        // Credential-independent: an unknown action must fail on its own
        // merits, not because there's no repo/token in the test environment.
        let mcp = AgentflareMcp::default();
        let req = GitHubRequest {
            action: "bogus".to_string(),
            ..Default::default()
        };
        let err = mcp.flare_git_impl(req).unwrap_err();
        assert!(err.to_string().contains("unknown action: bogus"), "{err}");
    }
}
