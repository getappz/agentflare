use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use flare_workflow::engine::WorkflowEngine;
use flare_workflow::executor::{noop_executor, FunctionStep};
use flare_workflow::store::InMemoryStore;
use flare_workflow::types::*;
use flare_workflow::{StepDefinition, WorkflowDefinition};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Ctx {
    calls: Vec<String>,
}

impl WorkflowData for Ctx {
    fn workflow_type() -> &'static str {
        "test"
    }
}

fn producer(id: String, output: &'static str) -> StepDefinition<Ctx> {
    StepDefinition::new(
        id.clone(),
        id.clone(),
        Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
            ctx.data.calls.push(id.to_string());
            ctx.output = output.to_string();
            Box::pin(async move { Ok(StepResult::Success) })
        })),
    )
}

fn echo(id: String) -> StepDefinition<Ctx> {
    StepDefinition::new(
        id.clone(),
        id.clone(),
        Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
            ctx.data.calls.push(id.to_string());
            ctx.output = format!("echo({})", ctx.input);
            Box::pin(async move { Ok(StepResult::Success) })
        })),
    )
}

fn engine() -> WorkflowEngine<Ctx, InMemoryStore<Ctx>> {
    WorkflowEngine::new()
}

async fn run_and_wait(
    engine: &WorkflowEngine<Ctx, InMemoryStore<Ctx>>,
    wf: WorkflowDefinition<Ctx>,
) -> WorkflowState<Ctx> {
    engine.register_workflow(wf).unwrap();
    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { calls: vec![] }, "seed".into())
        .await
        .unwrap();
    engine
        .wait_for_completion(run, "wf", Duration::from_secs(10))
        .await
        .unwrap();
    engine.get_status(run).await.unwrap()
}

#[tokio::test]
async fn conditional_skips_when_input_lacks_condition() {
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("first".into(), "all good"))
        .add_step(
            producer("only-if-error".into(), "fixed it")
                .with_mode(StepMode::Conditional { condition: "ERROR".into() }),
        );
    let state = run_and_wait(&engine(), wf).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(state.context.data.calls, vec!["first"]);
    assert_eq!(
        state.step_states[&StepId::new("only-if-error")].status,
        StepStatus::Skipped
    );
    // Input channel is unchanged (still the first step's output).
    assert_eq!(state.input, "all good");
}

#[tokio::test]
async fn conditional_executes_when_condition_met() {
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("first".into(), "Found an ERROR in the data"))
        .add_step(
            producer("fix".into(), "fixed").with_mode(StepMode::Conditional {
                condition: "ERROR".into(),
            }),
        );
    let state = run_and_wait(&engine(), wf).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(state.context.data.calls, vec!["first", "fix"]);
    assert_eq!(state.input, "fixed");
}

#[tokio::test]
async fn loop_until_condition_terminates_early() {
    let counter = Arc::new(AtomicU32::new(0));
    let cc = Arc::clone(&counter);
    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new(
            "refine",
            "refine",
            Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
                let n = cc.fetch_add(1, Ordering::SeqCst);
                ctx.data.calls.push(format!("iter{n}"));
                ctx.output = if n >= 2 { "Result: DONE".to_string() } else { "Still working...".to_string() };
                Box::pin(async move { Ok(StepResult::Success) })
            })),
        )
        .with_mode(StepMode::Loop {
            max_iterations: 5,
            until: "DONE".into(),
        }),
    );
    let state = run_and_wait(&engine(), wf).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    // 2 "working" iterations + 1 "DONE" = 3 iterations, early termination.
    assert_eq!(counter.load(Ordering::SeqCst), 3);
    assert_eq!(state.context.data.calls, vec!["iter0", "iter1", "iter2"]);
    assert!(state.input.contains("DONE"));
}

#[tokio::test]
async fn loop_respects_max_iterations() {
    let counter = Arc::new(AtomicU32::new(0));
    let cc = Arc::clone(&counter);
    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new(
            "refine",
            "refine",
            Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
                cc.fetch_add(1, Ordering::SeqCst);
                ctx.data.calls.push("iter".into());
                ctx.output = "iteration output".to_string();
                Box::pin(async move { Ok(StepResult::Success) })
            })),
        )
        .with_mode(StepMode::Loop {
            max_iterations: 3,
            until: "NEVER_MATCH".into(),
        }),
    );
    let state = run_and_wait(&engine(), wf).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(counter.load(Ordering::SeqCst), 3);
    assert_eq!(state.context.data.calls.len(), 3);
}

#[tokio::test]
async fn fan_out_collect_joins_outputs() {
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("task-a".into(), "Done: Task A").with_mode(StepMode::FanOut))
        .add_step(producer("task-b".into(), "Done: Task B").with_mode(StepMode::FanOut))
        .add_step(
            StepDefinition::new("collect", "collect", noop_executor::<Ctx>())
                .with_mode(StepMode::Collect),
        )
        .add_step(echo("synthesize".into()).depends_on(&["collect"]));
    let state = run_and_wait(&engine(), wf).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert!(state.input.contains("Done: Task A"));
    assert!(state.input.contains("Done: Task B"));
    assert!(state.input.contains("---"));
    // synthesize received the joined input.
    assert!(state.context.data.calls.contains(&"synthesize".to_string()));
}

#[tokio::test]
async fn output_variables_are_referenced_by_later_steps() {
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("research".into(), "alpha").with_output_var("research"))
        .add_step(producer("outline".into(), "beta").with_output_var("outline"))
        .add_step(echo("combine".into()).depends_on(&["outline"]));
    let state = run_and_wait(&engine(), wf).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    // combine's input is the previous step output ("beta"), not the vars.
    assert_eq!(state.input, "echo(beta)");
    // variables captured for reference.
    assert_eq!(state.variables.get("research").map(String::as_str), Some("alpha"));
    assert_eq!(state.variables.get("outline").map(String::as_str), Some("beta"));
}

#[tokio::test]
async fn error_mode_skip_continues_workflow() {
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(
            StepDefinition::new(
                "will-fail",
                "will-fail",
                Arc::new(FunctionStep::new(|_ctx: &mut WorkflowContext<Ctx>| {
                    Box::pin(async {
                        Err(WorkflowError::StepFailed {
                            step_id: StepId::new("will-fail"),
                            message: "simulated".into(),
                        })
                    })
                })),
            )
            .with_error_mode(ErrorMode::Skip),
        )
        .add_step(producer("succeeds".into(), "fine"));
    let state = run_and_wait(&engine(), wf).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    // Skip step left the input unchanged; the following step ran.
    assert_eq!(state.context.data.calls, vec!["succeeds"]);
}

#[tokio::test]
async fn error_mode_retry_succeeds_after_transient_failures() {
    let counter = Arc::new(AtomicU32::new(0));
    let cc = Arc::clone(&counter);
    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new(
            "flaky",
            "flaky",
            Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
                let n = cc.fetch_add(1, Ordering::SeqCst);
                let fail = n < 2;
                Box::pin(async move {
                    if fail {
                        Err(WorkflowError::StepFailed {
                            step_id: StepId::new("flaky"),
                            message: "transient".into(),
                        })
                    } else {
                        ctx.output = "finally worked".to_string();
                        Ok(StepResult::Success)
                    }
                })
            })),
        )
        .with_error_mode(ErrorMode::Retry { max_retries: 2 })
        .with_retry(flare_workflow::RetryPolicy {
            max_attempts: 4,
            backoff: BackoffStrategy::Fixed(Duration::from_millis(1)),
        }),
    );
    let state = run_and_wait(&engine(), wf).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(counter.load(Ordering::SeqCst), 3);
    assert_eq!(state.input, "finally worked");
}

#[tokio::test]
async fn token_accounting_recorded_on_success() {
    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new(
            "agent-step",
            "agent-step",
            Arc::new(FunctionStep::new(|ctx: &mut WorkflowContext<Ctx>| {
                ctx.output = "out".to_string();
                ctx.input_tokens = 100;
                ctx.output_tokens = 50;
                Box::pin(async move { Ok(StepResult::Success) })
            })),
        ),
    );
    let state = run_and_wait(&engine(), wf).await;
    let ss = &state.step_states[&StepId::new("agent-step")];
    assert_eq!(ss.input_tokens, 100);
    assert_eq!(ss.output_tokens, 50);
}
