use super::*;

impl AgentflareMcp {
    /// AI-agent-safe alternative to `agentflare work <id>` (which is
    /// human-only-gated): re-arms a stuck or failed item so the daemon's own
    /// supervisor discovery tick dispatches it, instead of the caller trying
    /// to dispatch it directly. See `agentflare_backend::item::redispatch`
    /// for exactly what it resets.
    pub(crate) fn item_redispatch(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = require_id(req.id, "redispatch")?;
        let req_assignee = req.assignee_agent.clone();
        let keep_pr = req.keep_pr.unwrap_or(false);
        let now = crate::claims::now();
        let ttl = crate::mcp_server::types::backend_claim_ttl_secs();
        self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
            // `keep_pr` is the caller's override; otherwise GitHub decides
            // (open PR stays tracked, merged/closed/missing is cleared).
            let outcome = agentflare_backend::item::redispatch(
                conn,
                &item_id,
                req_assignee.as_deref(),
                |number| {
                    if keep_pr {
                        agentflare_backend::item::PrRetention::Keep
                    } else {
                        crate::worktree::redispatch_pr_retention(
                            &self.worktree_repo_root(),
                            number,
                        )
                    }
                },
            )
            .map_err(map_backend_err)?;
            match outcome {
                agentflare_backend::item::RedispatchOutcome::Ready { assignee_agent } => {
                    // An explicit `assignee_agent` is a hand-off, same as
                    // `item_update`: release the previous agent's claim so the
                    // item isn't stuck non-dispatchable until the TTL lapses.
                    // Without one the assignee is unchanged, so a live claim
                    // still legitimately blocks dispatch.
                    let mut cancel_result = None;
                    if req_assignee.is_some() {
                        // Cancel first, release second: the old job's finalize
                        // step checks for cancellation before `item_done`, and
                        // once the claim is gone `item_done` would re-acquire
                        // it under the old owner. Flagging the job first means
                        // that check already sees the cancel by then.
                        cancel_result =
                            Some(self.cancel_jobs_for_reassignment(&item_id, &assignee_agent));
                        crate::claims::reassignment_releases_claim(
                            conn,
                            &item_id,
                            Some(&assignee_agent),
                        )
                        .map_err(internal_err)?;
                    }
                    let effective_ttl =
                        agentflare_backend::claim::effective_ttl_secs(conn, &item_id, ttl);
                    let live = agentflare_backend::claim::live_claim_on_item(
                        conn,
                        &item_id,
                        now,
                        effective_ttl,
                    )
                    .map_err(internal_err)?;
                    let blocked_by = live.filter(|c| c.owner != assignee_agent);
                    let resource_policy = agentflare_resource_gate::current_policy();
                    let resource_blocked = !matches!(
                        resource_policy,
                        agentflare_resource_gate::Policy::Normal
                            | agentflare_resource_gate::Policy::Aggressive
                    );
                    let dispatchable = blocked_by.is_none() && !resource_blocked;
                    let mut refusal_reason: Option<String> = None;
                    if let Some(ref c) = blocked_by {
                        refusal_reason = Some(format!(
                            "live claim held by agent '{}' (age {}s, ttl {}s)",
                            c.owner, c.age_secs, effective_ttl
                        ));
                    } else if resource_blocked {
                        refusal_reason = Some(format!(
                            "host resource gate policy is {:?} (host resource constrained)",
                            resource_policy
                        ));
                    }
                    let mut resp = serde_json::json!({
                        "item_id": item_id,
                        "ready": true,
                        "dispatchable": dispatchable,
                        "assignee_agent": assignee_agent,
                    });
                    if let Some(ref reason) = refusal_reason {
                        resp["refusal_reason"] = reason.as_str().into();
                    }
                    match cancel_result {
                        Some(Ok(n)) => resp["jobs_cancelled"] = n.into(),
                        Some(Err(e)) => resp["warning"] = job_cancel_warning(&e).into(),
                        None => {}
                    }
                    if let Some(live) = blocked_by {
                        resp["blocked_by_live_claim"] = serde_json::json!({
                            "owner": live.owner,
                            "age_secs": live.age_secs,
                            "ttl_secs": effective_ttl,
                            "reason": "another agent already holds a live claim on this item",
                        });
                    }
                    crate::quota::failover::attach_warning(
                        &mut resp,
                        crate::quota::failover::unavailability_warning(&assignee_agent),
                    );
                    Ok(resp.to_string())
                },
                agentflare_backend::item::RedispatchOutcome::NoAssignee => {
                    Err(ErrorData::invalid_params(
                        format!(
                            "item {item_id} has no assignee_agent to redispatch to -- pass one explicitly (assignee_agent=\"claude-code\") or set it first via item(action=\"update\")"
                        ),
                        None,
                    ))
                }
            }
        })?
    }
}
