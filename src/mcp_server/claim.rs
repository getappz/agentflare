//! `claim` MCP tool handler body -- split out of mcp_server.rs (item #168).

use super::*;

impl AgentflareMcp {
    /// Resolve a claim target that may be `item#<seq_id>` → `item#<uuid>`.
    fn resolve_claim_target(&self, target: &str) -> Result<String, ErrorData> {
        if let Some(rest) = target.strip_prefix("item#") {
            let uuid = self.with_backend_db(|conn| self.resolve_item_id(conn, rest))??;
            Ok(format!("item#{uuid}"))
        } else {
            Ok(target.to_string())
        }
    }

    pub fn claim_impl(&self, req: ClaimRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "acquire" => {
                let target = req
                    .target
                    .ok_or_else(|| ErrorData::invalid_params("target is required", None))?;
                let target = self.resolve_claim_target(&target)?;
                let repo_opt = req.repo;
                let repo_overridden = repo_opt.as_ref().is_some_and(|r| !r.is_empty());
                let (conn, repo) = Self::claim_ctx(&target, repo_opt)?;
                let owner = crate::claims::owner_id();
                let commit = if repo_overridden {
                    None
                } else {
                    Self::git_provenance().and_then(|g| g.commit)
                };
                let scope_arg = (!req.scope.is_empty()).then_some(req.scope.as_slice());
                let outcome = crate::claims::acquire(
                    &conn,
                    &repo,
                    &target,
                    &owner,
                    commit.as_deref(),
                    scope_arg,
                    crate::claims::now(),
                    crate::claims::ttl_secs(),
                )
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(match outcome {
                    crate::claims::Acquire::Acquired => {
                        let scope_warning = scope_arg.and_then(|s| {
                            crate::claims::scope_overlap_warning(
                                &conn,
                                &repo,
                                &target,
                                s,
                                crate::claims::now(),
                                crate::claims::ttl_secs(),
                            )
                            .ok()
                            .flatten()
                        });
                        serde_json::json!({ "status": "acquired", "repo": repo, "target": target, "owner": owner, "scope_warning": scope_warning })
                    }
                    crate::claims::Acquire::Held { owner: holder, age_secs } => serde_json::json!({ "status": "held", "repo": repo, "target": target, "owner": holder, "age_secs": age_secs }),
                }.to_string())
            }
            "heartbeat" => {
                let target = req
                    .target
                    .ok_or_else(|| ErrorData::invalid_params("target is required", None))?;
                let target = self.resolve_claim_target(&target)?;
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
                let target = self.resolve_claim_target(&target)?;
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
                let target = self.resolve_claim_target(&target)?;
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
