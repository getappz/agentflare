use super::*;
use crate::{
    FunctionStep, RetryPolicy, StepDefinition, StepResult, WorkflowContext, WorkflowDefinition,
    WorkflowRunId,
};
use rand::{SeedableRng, seq::SliceRandom};
use serde::Deserialize;
use std::{sync::Arc, time::Duration};

#[derive(Clone)]
enum Phase {
    Baseline,
    Propose(Vec<u32>),
    Probe(Vec<String>),
    Compose(Vec<u32>),
    Select,
    Reflect,
}

pub fn build_refinement_workflow(
    request: &RefinementRequest,
    backend: Arc<dyn RefinementBackend>,
) -> Result<WorkflowDefinition<RefinementData>, RefinementError> {
    request.validate()?;
    let mut phases = vec![Phase::Baseline];
    match request.mode {
        RefinementMode::Parallel => {
            let rounds: Vec<_> = (1..=request.k).collect();
            phases.push(Phase::Propose(rounds.clone()));
            phases.push(Phase::Probe(
                rounds
                    .iter()
                    .flat_map(|n| [format!("inv{n}"), format!("dom{n}")])
                    .collect(),
            ));
            phases.push(Phase::Compose(rounds.clone()));
            phases.push(Phase::Probe(
                rounds.iter().map(|n| format!("compose{n}")).collect(),
            ));
        }
        RefinementMode::Sequential => {
            for n in 1..=request.k {
                phases.push(Phase::Propose(vec![n]));
                phases.push(Phase::Probe(vec![format!("inv{n}"), format!("dom{n}")]));
                phases.push(Phase::Compose(vec![n]));
                phases.push(Phase::Probe(vec![format!("compose{n}")]));
            }
        }
    }
    phases.extend([Phase::Select, Phase::Reflect]);
    let id = format!("harness-refinement-{:?}-{}", request.mode, request.k);
    let mut definition = WorkflowDefinition::new(&id, &id);
    for (i, phase) in phases.into_iter().enumerate() {
        let backend = backend.clone();
        let step = FunctionStep::new(move |ctx: &mut WorkflowContext<RefinementData>| {
            let backend = backend.clone();
            let phase = phase.clone();
            Box::pin(async move {
                ctx.input_tokens = 0;
                ctx.output_tokens = 0;
                if ctx.data.completed_phases.contains(&i) {
                    ctx.output = serde_json::to_string(&ctx.data)
                        .map_err(|e| crate::WorkflowError::Store(e.to_string()))?;
                    return Ok(StepResult::Success);
                }
                let before = ctx.data.receipts.len();
                let result = execute_phase(&mut ctx.data, ctx.run_id, phase, backend).await;
                for r in &ctx.data.receipts[before..] {
                    ctx.input_tokens += r.input_tokens;
                    ctx.output_tokens += r.output_tokens;
                }
                let result = match result {
                    Ok(()) => {
                        ctx.data.completed_phases.insert(i);
                        StepResult::Success
                    }
                    Err(e) => {
                        ctx.data.adoption = None;
                        StepResult::Failed(e.to_string())
                    }
                };
                ctx.output = serde_json::to_string(&ctx.data)
                    .map_err(|e| crate::WorkflowError::Store(e.to_string()))?;
                Ok(result)
            })
        });
        let mut step = StepDefinition::new(
            format!("phase{i}"),
            format!("refinement phase {i}"),
            Arc::new(step),
        )
        .with_timeout(Duration::from_secs(
            request.limits.total_timeout_secs.saturating_add(30),
        ))
        .with_retry(RetryPolicy {
            max_attempts: 1,
            ..RetryPolicy::default()
        });
        if i > 0 {
            step = step.depends_on(&[&format!("phase{}", i - 1)]);
        }
        definition = definition.add_step(step);
    }
    Ok(definition)
}

fn invocation(
    data: &RefinementData,
    run: WorkflowRunId,
    id: String,
    role: Role,
    prompt: String,
    tree: HarnessTree,
) -> Invocation {
    Invocation {
        id,
        run_id: run,
        identity: data.identity.clone(),
        role,
        agent: if role == Role::Probe {
            data.request.solver.clone()
        } else {
            data.request.improver.clone()
        },
        prompt,
        harness: tree,
        checks: if role == Role::Probe {
            data.request.checks.clone()
        } else {
            vec![]
        },
        limits: data.request.limits.clone(),
    }
}

async fn call(
    backend: &Arc<dyn RefinementBackend>,
    inv: Invocation,
    receipts: &mut Vec<Receipt>,
) -> Result<Option<Receipt>, RefinementError> {
    for attempt in 1..=2 {
        let mut inv = inv.clone();
        inv.id = format!("{}.t{attempt}", inv.id);
        backend.check_active(inv.run_id).await?;
        let r = match backend.execute(inv.clone()).await {
            Ok(r) => r,
            Err(RefinementError::Transport(e) | RefinementError::Interrupted(e)) => Receipt {
                id: inv.id.clone(),
                error: Some(e),
                ..Receipt::default()
            },
            Err(e) => return Err(e),
        };
        if r.id != inv.id {
            return Err(RefinementError::Changed(
                "backend returned another invocation's receipt".into(),
            ));
        }
        let success = r.error.is_none();
        receipts.push(r.clone());
        if success {
            return Ok(Some(r));
        }
    }
    Ok(None)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    tree: HarnessTree,
    notes: String,
}

async fn stage(
    data: RefinementData,
    run: WorkflowRunId,
    label: String,
    role: Role,
    backend: Arc<dyn RefinementBackend>,
) -> (Stage, Vec<Receipt>, Option<RefinementError>) {
    let mut receipts = vec![];
    let mut error = String::new();
    for repair in 1..=2 {
        let prompt = if error.is_empty() {
            super::prompts::edit(&data, role)
        } else {
            format!(
                "{}\nPrevious attempt rejected: {error}. Fix the complete JSON tree.",
                super::prompts::edit(&data, role)
            )
        };
        match call(
            &backend,
            invocation(
                &data,
                run,
                format!("{label}.a{repair}"),
                role,
                prompt,
                data.current.clone(),
            ),
            &mut receipts,
        )
        .await
        {
            Ok(Some(receipt)) => match serde_json::from_str::<Proposal>(&receipt.reply) {
                Ok(proposal) => match validate_tree(&proposal.tree) {
                    Ok(()) => {
                        return (
                            Stage {
                                label,
                                tree: proposal.tree,
                                notes: proposal.notes,
                                fallback: false,
                                error: None,
                            },
                            receipts,
                            None,
                        );
                    }
                    Err(e) => error = e.to_string(),
                },
                Err(e) => error = format!("invalid proposal JSON: {e}"),
            },
            Ok(None) => {
                error = "transport attempts exhausted".into();
                break;
            }
            Err(e) => {
                return (
                    Stage {
                        label,
                        tree: data.current,
                        notes: String::new(),
                        fallback: true,
                        error: Some(e.to_string()),
                    },
                    receipts,
                    Some(e),
                );
            }
        }
    }
    (
        Stage {
            label,
            tree: data.current,
            notes: String::new(),
            fallback: true,
            error: Some(error),
        },
        receipts,
        None,
    )
}

async fn execute_phase(
    data: &mut RefinementData,
    run: WorkflowRunId,
    phase: Phase,
    backend: Arc<dyn RefinementBackend>,
) -> Result<(), RefinementError> {
    backend.check_active(run).await?;
    match phase {
        Phase::Baseline => {
            data.experiences = backend
                .load_experiences(&data.project(), &data.request.domain)
                .await?
                .into_iter()
                .filter(|e| e.project == data.project() && e.domain == data.request.domain)
                .collect();
            probes(
                data,
                run,
                (1..=data.request.k)
                    .map(|n| (format!("base{n}"), data.base.clone()))
                    .collect(),
                backend,
            )
            .await?;
        }
        Phase::Propose(rounds) => {
            let jobs = rounds
                .into_iter()
                .flat_map(|n| {
                    [
                        (format!("inv{n}"), Role::General),
                        (format!("dom{n}"), Role::Domain),
                    ]
                })
                .collect();
            stages(data, run, jobs, backend).await?;
        }
        Phase::Compose(rounds) => {
            stages(
                data,
                run,
                rounds
                    .iter()
                    .map(|n| (format!("compose{n}"), Role::Compose))
                    .collect(),
                backend,
            )
            .await?
        }
        Phase::Probe(labels) => {
            let mut jobs = vec![];
            for label in labels {
                let s = data
                    .stages
                    .iter()
                    .find(|s| s.label == label)
                    .ok_or_else(|| RefinementError::Invalid("missing stage".into()))?;
                jobs.push((label, s.tree.clone()));
            }
            probes(data, run, jobs, backend).await?;
            if data.request.mode == RefinementMode::Sequential
                && let Some(t) = data
                    .trials
                    .last()
                    .filter(|t| t.label.starts_with("compose"))
            {
                data.current = t.tree.clone();
            }
        }
        Phase::Select => select(data, run, backend).await?,
        Phase::Reflect => reflect(data, run, backend).await?,
    }
    Ok(())
}

async fn stages(
    data: &mut RefinementData,
    run: WorkflowRunId,
    jobs: Vec<(String, Role)>,
    backend: Arc<dyn RefinementBackend>,
) -> Result<(), RefinementError> {
    let mut pending = tokio::task::JoinSet::new();
    for (index, (label, role)) in jobs.into_iter().enumerate() {
        let snapshot = data.clone();
        let backend = backend.clone();
        pending.spawn(async move { (index, stage(snapshot, run, label, role, backend).await) });
    }
    let mut results = std::collections::BTreeMap::new();
    while let Some(result) = pending.join_next().await {
        let (index, value) = result.map_err(|e| RefinementError::Transport(e.to_string()))?;
        results.insert(index, value);
    }
    let mut fatal = None;
    for (_, (stage, receipts, error)) in results {
        data.stages.push(stage);
        data.receipts.extend(receipts);
        if error.is_some() {
            fatal = error;
        }
    }
    fatal.map_or(Ok(()), Err)
}
async fn probes(
    data: &mut RefinementData,
    run: WorkflowRunId,
    jobs: Vec<(String, HarnessTree)>,
    backend: Arc<dyn RefinementBackend>,
) -> Result<(), RefinementError> {
    let mut pending = tokio::task::JoinSet::new();
    for (index, (label, tree)) in jobs.into_iter().enumerate() {
        let inv = invocation(
            data,
            run,
            format!("{label}.probe"),
            Role::Probe,
            data.request.prompt.clone(),
            tree.clone(),
        );
        let backend = backend.clone();
        pending.spawn(async move {
            let mut receipts = vec![];
            let result = call(&backend, inv, &mut receipts).await;
            (index, label, tree, receipts, result)
        });
    }
    let mut results = std::collections::BTreeMap::new();
    while let Some(r) = pending.join_next().await {
        let (index, label, tree, receipts, result) =
            r.map_err(|e| RefinementError::Transport(e.to_string()))?;
        results.insert(index, (label, tree, receipts, result));
    }
    let mut fatal = None;
    for (_, (label, tree, receipts, result)) in results {
        let last_receipt = receipts.last().cloned();
        data.receipts.extend(receipts);
        match result {
            Ok(r) => data.trials.push(Trial {
                label,
                tree,
                receipt: r.or(last_receipt).unwrap_or_else(|| Receipt {
                    error: Some("transport attempts exhausted".into()),
                    ..Receipt::default()
                }),
            }),
            Err(e) => fatal = Some(e),
        }
    }
    fatal.map_or(Ok(()), Err)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Choice {
    choice: String,
    reason: String,
}
async fn select(
    data: &mut RefinementData,
    run: WorkflowRunId,
    backend: Arc<dyn RefinementBackend>,
) -> Result<(), RefinementError> {
    let mut eligible: Vec<_> = data
        .trials
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            t.receipt.successful()
                && (t.label.starts_with("base") || t.label.starts_with("compose"))
        })
        .map(|(i, _)| i)
        .collect();
    if eligible.is_empty() {
        data.warnings
            .push("No successful baseline or final; no adoption".into());
        return Ok(());
    }
    let seed = digest(data.identity.as_bytes());
    let seed = u64::from_str_radix(&seed[..16], 16).unwrap_or(0);
    eligible.shuffle(&mut rand::rngs::StdRng::seed_from_u64(seed));
    let pool: Vec<_> = eligible
        .iter()
        .enumerate()
        .map(|(i, index)| (format!("candidate{}", i + 1), *index))
        .collect();
    let fallback = data
        .trials
        .iter()
        .position(|t| t.label.starts_with("base") && t.receipt.successful());
    let inv = invocation(
        data,
        run,
        "select".into(),
        Role::Select,
        super::prompts::selection(data, &pool),
        data.base.clone(),
    );
    let result = call(&backend, inv, &mut data.receipts).await?;
    let mut reason = "Inconclusive selection; retained successful baseline".to_string();
    let chosen = match result.and_then(|r| serde_json::from_str::<Choice>(&r.reply).ok()) {
        Some(c) if c.choice == "abstain" => {
            reason = c.reason;
            fallback
        }
        Some(c) if !c.reason.trim().is_empty() => match pool.iter().find(|(id, _)| *id == c.choice)
        {
            Some((_, index)) => {
                reason = c.reason;
                Some(*index)
            }
            None => {
                data.warnings
                    .push("Selector returned an unknown candidate".into());
                fallback
            }
        },
        _ => {
            data.warnings.push(
                "Selector failed or returned invalid JSON; retained baseline when available".into(),
            );
            fallback
        }
    };
    backend.check_active(run).await?;
    data.adoption = chosen.map(|index| {
        let t = &data.trials[index];
        Adoption {
            trial_id: t.label.clone(),
            harness_identity: harness_identity(&t.tree),
            reason,
            is_baseline: t.label.starts_with("base"),
        }
    });
    if let Some(adoption) = &data.adoption {
        data.current = data
            .trials
            .iter()
            .find(|t| t.label == adoption.trial_id)
            .expect("selected trial exists")
            .tree
            .clone();
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lesson {
    cue: String,
    lesson: String,
    evidence: String,
    trial_ids: Vec<String>,
    confidence: String,
}
async fn reflect(
    data: &mut RefinementData,
    run: WorkflowRunId,
    backend: Arc<dyn RefinementBackend>,
) -> Result<(), RefinementError> {
    if data.adoption.is_none() {
        return Ok(());
    }
    let inv = invocation(
        data,
        run,
        "reflect".into(),
        Role::Reflect,
        super::prompts::reflection(data),
        data.base.clone(),
    );
    let result = match call(&backend, inv, &mut data.receipts).await {
        Ok(r) => r,
        Err(RefinementError::Cancelled) => return Err(RefinementError::Cancelled),
        Err(e) => {
            data.warnings.push(format!("Reflection unavailable: {e}"));
            return Ok(());
        }
    };
    let lesson = result.and_then(|r| {
        serde_json::from_str::<Option<Lesson>>(&r.reply)
            .ok()
            .flatten()
    });
    if let Some(l) = lesson {
        let experience = Experience {
            project: data.project(),
            domain: data.request.domain.clone(),
            cue: l.cue,
            lesson: l.lesson,
            evidence: l.evidence,
            trial_ids: l.trial_ids,
            confidence: l.confidence,
        };
        if experience.valid_for(data) {
            backend.check_active(run).await?;
            if let Err(e) = backend.save_experience(run, &experience).await {
                data.warnings
                    .push(format!("Experience persistence failed: {e}"));
            }
        } else {
            data.warnings
                .push("Reflection lacked observed supporting evidence".into());
        }
    } else {
        data.warnings
            .push("Reflection returned no valid transferable lesson".into());
    }
    Ok(())
}
