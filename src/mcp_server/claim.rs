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

    /// Confirms `owner` can act on `(repo, target)` before a `release`/`done`
    /// call, steals the lease if it's abandoned (stale or never claimed), and
    /// errors loudly if someone else's live claim is in the way — the same
    /// fix `item_release`/`item_done` already apply to the `item_claims`
    /// ledger (item #83), applied here to this tool's own `claims` ledger.
    /// Without this, a claim whose owner doesn't match the caller's current
    /// `owner_id()` (e.g. because it was created by a different process/
    /// session — see `claims::owner_id`'s doc comment) left `release`/`done`
    /// silently returning `false` with no way to tell "nothing to release"
    /// apart from "someone else is actively using this", and no path to
    /// actually let go of a claim attributed to the caller but not created
    /// by the caller's own session.
    fn claim_confirm_or_steal(
        conn: &rusqlite::Connection,
        repo: &str,
        target: &str,
        owner: &str,
        now: i64,
        ttl: i64,
    ) -> Result<(), ErrorData> {
        if let crate::claims::Acquire::Held {
            owner: holder,
            age_secs,
        } = crate::claims::acquire(conn, repo, target, owner, None, None, now, ttl)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
        {
            return Err(ErrorData::invalid_params(
                format!(
                    "target '{target}' in repo '{repo}' is claimed by '{holder}' (active {age_secs}s ago, ttl {ttl}s) -- refusing to release someone else's live claim"
                ),
                None,
            ));
        }
        Ok(())
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
                let clear_warning =
                    crate::claims::scope_clear_warning(&conn, &repo, &target, scope_arg)
                        .ok()
                        .flatten();
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
                        let scope_warning = clear_warning.or_else(|| {
                            scope_arg.and_then(|s| {
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
                            })
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
                let now = crate::claims::now();
                let ttl = crate::claims::ttl_secs();
                Self::claim_confirm_or_steal(&conn, &repo, &target, &owner, now, ttl)?;
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
                let now = crate::claims::now();
                let ttl = crate::claims::ttl_secs();
                Self::claim_confirm_or_steal(&conn, &repo, &target, &owner, now, ttl)?;
                let ok = crate::claims::done(&conn, &repo, &target, &owner, now)
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
