use crate::decide::{self, DecideError, Outcome, shadow};
use crate::optimize::RouteContext;
use crate::skill_detect::RankedSkill;
use crate::skill_rerank;
use std::time::Duration;

pub(crate) struct Result {
    pub route_nudge: Option<String>,
    pub skill_pick: Option<Vec<RankedSkill>>,
}

pub(crate) fn run(
    prompt: &str,
    jev_route_ctx: Option<&RouteContext>,
    pending_rerank: Option<&skill_rerank::Pending>,
) -> Result {
    run_with(
        prompt,
        jev_route_ctx,
        pending_rerank,
        &mut |batch| batch.ask_within(Duration::from_millis(1500)),
        &shadow::record,
    )
}

fn run_with(
    prompt: &str,
    jev_route_ctx: Option<&RouteContext>,
    pending_rerank: Option<&skill_rerank::Pending>,
    ask: &mut dyn FnMut(&decide::Batch) -> std::result::Result<Outcome, DecideError>,
    record: &dyn Fn(&shadow::Row),
) -> Result {
    let mut batch = decide::Batch::new(prompt);
    let batch_route = jev_route_ctx
        .map(|ctx| crate::optimize::jev_router::add_to_batch(ctx, &mut batch))
        .unwrap_or(false);
    if let Some(pending) = pending_rerank {
        pending.add_to_batch(&mut batch, "skill_rerank.");
    }
    if batch.is_empty() {
        return Result {
            route_nudge: None,
            skill_pick: None,
        };
    }

    match ask(&batch) {
        Ok(outcome) => Result {
            route_nudge: match (batch_route, jev_route_ctx) {
                (true, Some(ctx)) => {
                    crate::optimize::jev_router::route_batch_success(ctx, &outcome, record)
                }
                _ => None,
            },
            skill_pick: pending_rerank
                .map(|pending| pending.finish_success(&outcome, "skill_rerank.", record)),
        },
        Err(err) => Result {
            route_nudge: match (batch_route, jev_route_ctx) {
                (true, Some(ctx)) => {
                    crate::optimize::jev_router::route_batch_error(ctx, &err, record)
                }
                _ => None,
            },
            skill_pick: pending_rerank.map(|pending| pending.finish_error(&err, record)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::{Answer, Response, Usage};
    use crate::optimize::ToolCallRecord;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    fn ctx(prompt: &str) -> RouteContext {
        RouteContext {
            prompt: prompt.to_string(),
            session_id: "s1".to_string(),
            turn_count: 1,
            recent_tool_calls: Vec::<ToolCallRecord>::new(),
            current_model: Some("sonnet".to_string()),
        }
    }

    fn skill(name: &str) -> RankedSkill {
        RankedSkill {
            est_tokens: 100,
            name: name.to_string(),
            source: "test".to_string(),
            description: format!("{name} skill"),
            score: 1.0,
            match_reason: "test".to_string(),
        }
    }

    fn pending() -> skill_rerank::Pending {
        let ranked = vec![skill("alpha"), skill("beta")];
        skill_rerank::prepare_for_test("fix a bug", ranked.clone(), ranked, 1).unwrap()
    }

    fn outcome(ids: &[String]) -> Outcome {
        let answers = ids
            .iter()
            .map(|id| {
                let answer = if id == "router.difficulty" {
                    Answer::Choice {
                        choice: "easy".to_string(),
                        confidence: Some(0.95),
                        probabilities: BTreeMap::new(),
                    }
                } else if id == "skill_rerank.s0" {
                    Answer::Noul { noul: 0.95 }
                } else {
                    Answer::Noul { noul: 0.05 }
                };
                (id.clone(), answer)
            })
            .collect();
        Outcome {
            response: Response {
                answers,
                model: Some("typesafe/jev".to_string()),
                usage: Usage::default(),
            },
            elapsed_ms: 7,
        }
    }

    #[test]
    fn both_consumers_share_one_ask_and_map_answers() {
        let route_ctx = ctx("fix a bug");
        let rerank = pending();
        let calls = RefCell::new(Vec::<Vec<String>>::new());
        let rows = RefCell::new(Vec::<shadow::Row>::new());

        let result = run_with(
            "fix a bug",
            Some(&route_ctx),
            Some(&rerank),
            &mut |batch| {
                let ids = batch.questions().keys().cloned().collect::<Vec<_>>();
                calls.borrow_mut().push(ids.clone());
                Ok(outcome(&ids))
            },
            &|row| rows.borrow_mut().push(row.clone()),
        );

        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(
            calls.borrow()[0],
            vec![
                "router.difficulty".to_string(),
                "skill_rerank.s0".to_string(),
                "skill_rerank.s1".to_string(),
            ]
        );
        assert!(result.route_nudge.unwrap().contains("haiku"));
        assert_eq!(result.skill_pick.unwrap()[0].name, "alpha");
        assert_eq!(
            rows.borrow()
                .iter()
                .map(|row| row.site.as_str())
                .collect::<Vec<_>>(),
            vec!["router", "skill_rerank"]
        );
    }

    #[test]
    fn one_consumer_sends_only_its_questions() {
        let route_ctx = ctx("fix a bug");
        let mut seen = Vec::new();
        let route_only = run_with(
            "fix a bug",
            Some(&route_ctx),
            None,
            &mut |batch| {
                seen.push(batch.questions().keys().cloned().collect::<Vec<_>>());
                Ok(outcome(seen.last().unwrap()))
            },
            &|_| {},
        );
        assert!(route_only.skill_pick.is_none());
        assert_eq!(seen, vec![vec!["router.difficulty".to_string()]]);

        let rerank = pending();
        seen.clear();
        let rerank_only = run_with(
            "fix a bug",
            None,
            Some(&rerank),
            &mut |batch| {
                seen.push(batch.questions().keys().cloned().collect::<Vec<_>>());
                Ok(outcome(seen.last().unwrap()))
            },
            &|_| {},
        );
        assert!(rerank_only.route_nudge.is_none());
        assert_eq!(
            seen,
            vec![vec![
                "skill_rerank.s0".to_string(),
                "skill_rerank.s1".to_string(),
            ]]
        );
    }

    #[test]
    fn neither_consumer_skips_ask() {
        let mut calls = 0;
        let result = run_with(
            "fix a bug",
            None,
            None,
            &mut |_| {
                calls += 1;
                Err(DecideError::Transport("unexpected".to_string()))
            },
            &|_| {},
        );
        assert_eq!(calls, 0);
        assert!(result.route_nudge.is_none());
        assert!(result.skill_pick.is_none());
    }

    #[test]
    fn failure_fans_out_to_each_enabled_fallback() {
        let route_ctx = ctx("fix a bug");
        let rerank = pending();
        let rows = RefCell::new(Vec::<shadow::Row>::new());

        let result = run_with(
            "fix a bug",
            Some(&route_ctx),
            Some(&rerank),
            &mut |_| Err(DecideError::Transport("down".to_string())),
            &|row| rows.borrow_mut().push(row.clone()),
        );

        assert!(result.route_nudge.is_none());
        assert_eq!(result.skill_pick.unwrap()[0].name, "alpha");
        assert_eq!(
            rows.borrow()
                .iter()
                .map(|row| (row.site.as_str(), row.error.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("router", Some("decision request failed: down")),
                ("skill_rerank", Some("decision request failed: down")),
            ]
        );
    }
}
