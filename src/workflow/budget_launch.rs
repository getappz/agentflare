use std::time::Duration;

use flare_workflow::budget::{BudgetAllocation, error};

use crate::claude_cost::Launch;

/// A finished launch's outcome with the cost of its turn. Runs on the blocking thread right after
/// the agent returns, so a completed turn's session total is kept in the ledger even when the
/// caller's future was dropped by a timeout. For a Claude Code reply (`claude_launch` is `Some`)
/// the reported total is the session's running total, so the turn's own cost comes from the ledger.
pub(crate) fn settle(
    claude_launch: Option<Launch>,
    outcome: crate::agent_launch::HeadlessOutcome,
) -> (crate::agent_launch::HeadlessOutcome, Option<f64>) {
    let cost = match &outcome {
        crate::agent_launch::HeadlessOutcome::Ok(reply) => turn_cost(claude_launch.as_ref(), reply),
        _ => None,
    };
    (outcome, cost)
}

fn turn_cost(
    claude_launch: Option<&Launch>,
    reply: &crate::agent_launch::HeadlessReply,
) -> Option<f64> {
    if reply.is_error && reply.cost_usd == Some(0.0) {
        return None;
    }
    let Some(launch) = claude_launch else {
        return reply.cost_usd;
    };
    let (session, total) = reply.session_id.as_deref().zip(reply.cost_usd)?;
    crate::claude_cost::turn_cost(&crate::claude_cost::default_path(), session, total, launch)
}

pub(crate) fn record_reply(allocation: Option<&BudgetAllocation>, cost: Option<f64>) {
    if let Some(allocation) = allocation {
        allocation.record_cost(cost);
    }
}

pub(crate) fn prepare(
    agent: &str,
    args: &mut Vec<String>,
    hard_cap: &mut Duration,
    allocation: Option<&BudgetAllocation>,
) -> Result<(), String> {
    let Some(allocation) = allocation else {
        return Ok(());
    };
    if let Some(max) = allocation.max_cost_usd()
        && let Err(error) = cap_args(agent, args, max)
    {
        allocation.record_cost(Some(0.0));
        return Err(error);
    }
    if let Some(remaining) = allocation.remaining_time().map_err(|e| e.to_string())? {
        *hard_cap = (*hard_cap).min(remaining);
    }
    Ok(())
}

fn cap_args(agent: &str, args: &mut Vec<String>, mut max: f64) -> Result<(), String> {
    if agent_registry::agent_by_name(agent) != Some(agent_registry::Agent::ClaudeCode) {
        return Err(error(format!(
            "dollar budgets require a provider-enforced cap; {agent} is unsupported"
        ))
        .to_string());
    }
    if args.iter().any(|arg| arg == "--") {
        return Err(
            error("dollar budgets do not allow a positional-argument delimiter").to_string(),
        );
    }
    let mut retained = Vec::with_capacity(args.len() + 1);
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let value = if arg == "--max-budget-usd" {
            Some(
                iter.next()
                    .ok_or_else(|| error("missing --max-budget-usd value").to_string())?
                    .as_str(),
            )
        } else {
            arg.strip_prefix("--max-budget-usd=")
        };
        if let Some(value) = value {
            let existing: f64 = value
                .parse()
                .map_err(|_| error("invalid --max-budget-usd value").to_string())?;
            if !existing.is_finite() || existing < 0.000001 {
                return Err(error("--max-budget-usd must be finite and positive").to_string());
            }
            max = max.min((existing * 1_000_000.0).floor() / 1_000_000.0);
        } else {
            retained.push(arg.clone());
        }
    }
    retained.insert(0, format!("--max-budget-usd={max:.6}"));
    *args = retained;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn resumed_totals_and_zero_cost_errors_keep_their_reservations() {
        use flare_workflow::budget::BudgetOperation;
        use flare_workflow::{PipelineData, StateStore, WorkflowEngine, WorkflowId};
        for (cumulative, is_error, cost, reported, held) in [
            (false, false, 0.1, 0.1, 0.0),
            (true, false, 0.8, 0.0, 0.5),
            (false, true, 0.0, 0.0, 0.5),
        ] {
            let store = flare_workflow::SqliteStore::<PipelineData>::open_memory().unwrap();
            let engine = WorkflowEngine::with_store(store.clone());
            let send: flare_workflow::SendMessage = std::sync::Arc::new(move |inv| {
                Box::pin(async move {
                    let reply = crate::agent_launch::HeadlessReply {
                        cost_usd: Some(cost),
                        is_error,
                        ..Default::default()
                    };
                    let cost = turn_cost(cumulative.then(Launch::default).as_ref(), &reply);
                    record_reply(inv.allocation.as_ref(), cost);
                    Ok(("done".into(), 0, 0))
                })
            });
            let json: flare_workflow::JsonWorkflow = serde_json::from_value(serde_json::json!({"name":"receipt","budget":{"max_cost_usd":1.0,"reserve_per_call_usd":0.5},"steps":[{"name":"call","agent":"fixture"}]})).unwrap();
            engine
                .register_workflow(flare_workflow::compile_workflow(&json, send).unwrap())
                .unwrap();
            let run = engine
                .start_workflow(WorkflowId::new("receipt"), PipelineData, String::new())
                .await
                .unwrap();
            engine
                .wait_for_completion(run, "test", Duration::from_secs(5))
                .await
                .unwrap();
            let summary = store
                .budget_operation(run, BudgetOperation::Snapshot)
                .await
                .unwrap()
                .state
                .summary();
            assert_eq!(summary["reported_cost_usd"], reported);
            assert_eq!(summary["held_cost_usd"], held);
        }
    }
    #[test]
    fn shared_cap_cannot_be_overridden_and_preserves_a_stricter_existing_cap() {
        let mut args = vec![
            "--max-budget-usd=9".into(),
            "--model".into(),
            "sonnet".into(),
            "--max-budget-usd".into(),
            "0.2".into(),
        ];
        cap_args("claude-code", &mut args, 0.5).unwrap();
        assert_eq!(args, ["--max-budget-usd=0.200000", "--model", "sonnet"]);
    }
    #[test]
    fn unsupported_providers_and_invalid_existing_caps_are_rejected() {
        assert!(cap_args("codex", &mut Vec::new(), 0.5).is_err());
        assert!(cap_args("claude-code", &mut vec!["--".into(), "prompt".into()], 0.5).is_err());
        for value in ["NaN", "inf", "0", "-1", "garbage", "0.0000001"] {
            assert!(
                cap_args(
                    "claude-code",
                    &mut vec![format!("--max-budget-usd={value}")],
                    0.5
                )
                .is_err()
            );
        }
        assert!(cap_args("claude-code", &mut vec!["--max-budget-usd".into()], 0.5).is_err());
    }
}
