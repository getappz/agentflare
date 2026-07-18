//! `review` MCP tool handler body -- split out of mcp_server.rs (item #168).

use super::*;

impl AgentflareMcp {
    pub fn review_impl(&self, req: ReviewRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "submit" => {
                let findings = req
                    .findings
                    .ok_or_else(|| ErrorData::invalid_params("findings is required", None))?;
                let conn = Self::claim_db()?;
                let repo = Self::resolve_repo_or_err(req.repo)?;
                let pr = Self::resolve_round(req.pr)?;
                // SECURITY / step-3 classification (#75): the finder `agent`
                // stays caller-settable BY DESIGN. Unlike artifact authorship,
                // review findings live in a local, per-repo, single-user DB, and
                // a `/code-review` orchestrator legitimately submits on behalf
                // of many finder sub-agents — consensus counts DISTINCT finder
                // names, so collapsing them to one server identity would break
                // it. No cross-principal trust boundary exists here; the
                // server-derived `submitter_name` is the fallback when unset.
                let agent = req
                    .agent
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(crate::review::submitter_name);
                let parsed: Vec<crate::review::Finding> = findings
                    .into_iter()
                    .map(serde_json::from_value)
                    .collect::<Result<_, _>>()
                    .map_err(|e| {
                        ErrorData::invalid_params(format!("invalid finding: {e}"), None)
                    })?;
                let n =
                    crate::review::submit(&conn, &repo, &pr, &agent, &parsed, crate::claims::now())
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(
                    serde_json::json!({ "submitted": n, "repo": repo, "pr": pr, "agent": agent })
                        .to_string(),
                )
            }
            "consensus" => {
                let conn = Self::claim_db()?;
                let repo = Self::resolve_repo_or_err(req.repo)?;
                let pr = Self::resolve_round(req.pr)?;
                let findings = crate::review::load(&conn, &repo, &pr)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let diff = crate::review::compute_diff(req.base.as_deref(), req.head.as_deref())
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let changed = crate::review::changed_lines(&diff);
                let result = crate::review::consensus(&findings, &changed);
                Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
            }
            "list" => {
                let conn = Self::claim_db()?;
                let repo = Self::resolve_repo_or_err(req.repo)?;
                let pr = Self::resolve_round(req.pr)?;
                let findings = crate::review::load(&conn, &repo, &pr)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let rows: Vec<serde_json::Value> = findings.iter().map(|sf| serde_json::json!({ "agent": sf.agent, "file": sf.finding.file, "line": sf.finding.line, "message": sf.finding.message, "severity": sf.finding.severity })).collect();
                Ok(serde_json::to_string_pretty(&rows).unwrap_or_default())
            }
            "clear" => {
                let conn = Self::claim_db()?;
                let repo = Self::resolve_repo_or_err(req.repo)?;
                let pr = Self::resolve_round(req.pr)?;
                crate::review::clear(&conn, &repo, &pr)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(serde_json::json!({"cleared": true}).to_string())
            }
            "record" => {
                let conn = Self::claim_db()?;
                let repo = Self::resolve_repo_or_err(req.repo)?;
                let pr = Self::resolve_round(req.pr)?;
                let findings = crate::review::load(&conn, &repo, &pr)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let diff = crate::review::compute_diff(req.base.as_deref(), req.head.as_deref())
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let changed = crate::review::changed_lines(&diff);
                let n = crate::review::record_round(
                    &conn,
                    &repo,
                    &pr,
                    &findings,
                    &changed,
                    crate::claims::now(),
                )
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(serde_json::json!({ "recorded_agents": n, "repo": repo, "pr": pr }).to_string())
            }
            "scores" => {
                let conn = Self::claim_db()?;
                let repo = req.repo;
                let all_repos = req.all_repos;
                let scope = if all_repos {
                    None
                } else {
                    Some(Self::resolve_repo_or_err(repo)?)
                };
                let scores = crate::review::scores(&conn, scope.as_deref())
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(serde_json::to_string_pretty(&scores).unwrap_or_default())
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }
}
