//! `claim` MCP tool handler body -- split out of mcp_server.rs (item #168).

use super::*;

impl AgentflareMcp {
    pub fn claim_impl(&self, req: ClaimRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "acquire" => {
                let target = req
                    .target
                    .ok_or_else(|| ErrorData::invalid_params("target is required", None))?;
                let repo_opt = req.repo;
                let repo_overridden = repo_opt.as_ref().is_some_and(|r| !r.is_empty());
                let (conn, repo) = Self::claim_ctx(&target, repo_opt)?;
                let owner = crate::claims::owner_id();
                let commit = if repo_overridden {
                    None
                } else {
                    Self::git_provenance().and_then(|g| g.commit)
                };
                let outcome = crate::claims::acquire(
                    &conn,
                    &repo,
                    &target,
                    &owner,
                    commit.as_deref(),
                    crate::claims::now(),
                    crate::claims::ttl_secs(),
                )
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(match outcome {
                    crate::claims::Acquire::Acquired => serde_json::json!({ "status": "acquired", "repo": repo, "target": target, "owner": owner }),
                    crate::claims::Acquire::Held { owner: holder, age_secs } => serde_json::json!({ "status": "held", "repo": repo, "target": target, "owner": holder, "age_secs": age_secs }),
                }.to_string())
            }
            "heartbeat" => {
                let target = req
                    .target
                    .ok_or_else(|| ErrorData::invalid_params("target is required", None))?;
                let (conn, repo) = Self::claim_ctx(&target, req.repo)?;
                let owner = crate::claims::owner_id();
                let ok =
                    crate::claims::heartbeat(&conn, &repo, &target, &owner, crate::claims::now())
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(
                    serde_json::json!({ "refreshed": ok, "repo": repo, "target": target })
                        .to_string(),
                )
            }
            "release" => {
                let target = req
                    .target
                    .ok_or_else(|| ErrorData::invalid_params("target is required", None))?;
                let (conn, repo) = Self::claim_ctx(&target, req.repo)?;
                let owner = crate::claims::owner_id();
                let ok = crate::claims::release(&conn, &repo, &target, &owner)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(
                    serde_json::json!({ "released": ok, "repo": repo, "target": target })
                        .to_string(),
                )
            }
            "done" => {
                let target = req
                    .target
                    .ok_or_else(|| ErrorData::invalid_params("target is required", None))?;
                let (conn, repo) = Self::claim_ctx(&target, req.repo)?;
                let owner = crate::claims::owner_id();
                let ok = crate::claims::done(&conn, &repo, &target, &owner, crate::claims::now())
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(serde_json::json!({ "done": ok, "repo": repo, "target": target }).to_string())
            }
            "list" => {
                let conn = Self::claim_db()?;
                let scope = if req.all_repos {
                    None
                } else {
                    Some(crate::claims::resolve_repo(req.repo).ok_or_else(|| ErrorData::invalid_params("could not determine repo — run in a git repo or pass repo=owner/name (or all_repos=true)", None))?)
                };
                let claims = crate::claims::list(
                    &conn,
                    scope.as_deref(),
                    req.all,
                    crate::claims::now(),
                    crate::claims::ttl_secs(),
                )
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(serde_json::to_string_pretty(&claims).unwrap_or_default())
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }
}
