//! `autopilot` action `next_work`: pick and claim the next ready item.
//!
//! Eligible = labelled `ready-for-work`, open, assigned to `req.agent` (same
//! `agent_part` matching as the supervisor, so `claude-code:<job>` counts),
//! and with no open `blocks` dependency. Lowest `sequence_id` first.
//! Plan-gated and claim-held items are passed over; if nothing is claimable
//! the first such item is reported with its `blocked_reason`.
use super::*;
use agentflare_backend::item::plan_gate::{PlanGateStatus, plan_gate_status};

/// Repo key the ledger claim is filed under (origin, else a stable local key).
pub(super) fn claim_repo() -> String {
    crate::claims::resolve_repo(None).unwrap_or_else(|| "local".to_string())
}

fn item_json(i: &agentflare_backend::item::Item) -> serde_json::Value {
    serde_json::json!({
        "id": i.id, "seq": i.sequence_id, "title": i.name, "description": i.description,
    })
}

impl AgentflareMcp {
    pub(super) fn autopilot_next_work(
        &self,
        req: &AutopilotRequest,
    ) -> Result<serde_json::Value, ErrorData> {
        let bad = |m: &str| ErrorData::invalid_params(m.to_string(), None);
        if req.max_cost_usd.is_some_and(|c| c.is_nan() || c < 0.0) {
            return Err(bad("max_cost_usd must be >= 0"));
        }
        if req
            .rate_limit_pct
            .is_some_and(|p| !(0.0..=100.0).contains(&p))
        {
            return Err(bad("rate_limit_pct must be within 0..=100"));
        }
        let agent = req
            .agent
            .as_deref()
            .filter(|a| !a.is_empty())
            .ok_or_else(|| bad("agent is required for next_work"))?;
        let want = agentflare_backend::item::agent_part(agent);

        let candidates = self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            let Ok(label) =
                agentflare_backend::label::get_by_name(conn, &project.id, "ready-for-work")
            else {
                return Ok::<_, ErrorData>(Vec::new());
            };
            let mut items = agentflare_backend::item::list_by_label(conn, &project.id, &label.id)
                .map_err(map_backend_err)?;
            items.retain(|i| {
                i.assignee_agent
                    .as_deref()
                    .is_some_and(|a| agentflare_backend::item::agent_part(a) == want)
            });
            items.sort_by_key(|i| i.sequence_id);
            let ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();
            let edges = agentflare_backend::item::dependency_edges_for_items(conn, &ids)
                .map_err(map_backend_err)?;
            items.retain(|i| {
                !edges.iter().any(|(id, _, group)| {
                    id == &i.id && group != "completed" && group != "cancelled"
                })
            });
            Ok(items)
        })??;

        let me = crate::claims::owner_id();
        let mut blocked: Option<(&str, &agentflare_backend::item::Item)> = None;
        for item in &candidates {
            if matches!(plan_gate_status(&item.metadata), PlanGateStatus::Blocked(_)) {
                blocked.get_or_insert(("plan_approval", item));
                continue;
            }
            let out = self.claim_impl(ClaimRequest {
                action: "acquire".into(),
                target: Some(format!("item#{}", item.id)),
                repo: Some(claim_repo()),
                ..Default::default()
            })?;
            let v: serde_json::Value = serde_json::from_str(&out)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            // `held` comes back as a success payload: compare the holder to us.
            if v["status"] == "acquired" || v["owner"] == me.as_str() {
                return Ok(serde_json::json!({ "item": item_json(item), "claimed": true }));
            }
            blocked.get_or_insert(("claim_held", item));
        }
        Ok(match blocked {
            Some((reason, item)) => serde_json::json!({
                "item": item_json(item), "claimed": false, "blocked_reason": reason,
            }),
            None => serde_json::json!({ "claimed": false, "blocked_reason": "none_ready" }),
        })
    }
}
