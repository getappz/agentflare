//! `StepMode::Loop` execution: repeats a step's executor until its output
//! contains the configured `until` substring or `max_iterations` elapse.
//!
//! Design: `LOOP_DURABILITY_DESIGN.md` (item #115). Mirrors `waits.rs` /
//! `rollback.rs`: durable-shaped methods on `WorkflowEngine<D, S>` in their
//! own module. Each successful iteration is journaled as a
//! `JournalEntry::LoopIteration` so a crash mid-loop resumes from the last
//! recorded iteration instead of restarting the counter at 1.

use chrono::Utc;
use tokio::time::timeout;

use crate::definition::{StepDefinition, WorkflowDefinition};
use crate::engine::WorkflowEngine;
use crate::retry::{self, Backoff};
use crate::store::StateStore;
use crate::types::*;
use crate::variables::capture_output;

impl<D: WorkflowData, S: StateStore<D> + 'static> WorkflowEngine<D, S> {
    /// Execute a `Loop` step: repeat the executor until the output contains
    /// `until` or `max_iterations` elapse, chaining each iteration's output
    /// back as the next `{{input}}`.
    pub(crate) async fn execute_loop(
        &self,
        run_id: WorkflowRunId,
        step: &StepDefinition<D>,
        definition: &WorkflowDefinition<D>,
    ) -> WorkflowResult<StepResult> {
        let StepMode::Loop {
            max_iterations,
            until,
        } = &step.mode
        else {
            return Ok(StepResult::Skip);
        };
        let step_timeout = definition.get_timeout(step);
        let until_lower = until.to_lowercase();
        let mut last_context: Option<WorkflowContext<D>> = None;
        let journal = self.state_store.journal(run_id).await?;
        let resume_from = journal
            .iter()
            .filter_map(|e| match e {
                JournalEntry::LoopIteration {
                    step_id, iteration, ..
                } if step_id == &step.id => Some(*iteration),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let mut executed = resume_from;

        for iter in (resume_from + 1)..=*max_iterations {
            if self.state_store.is_cancelled(run_id).await? {
                return Err(WorkflowError::Cancelled(run_id));
            }

            let state = self.state_store.load(run_id).await?;
            let mut context = state.context.clone();
            context.input = state.input.clone();
            context.variables = state.variables.clone();
            context.output.clear();
            context.step = StepExecutionMeta {
                workflow_id: definition.id.to_string(),
                workflow_name: definition.name.clone(),
                step_id: step.id.to_string(),
                step_name: step.name.clone(),
                attempt: iter,
                max_attempts: *max_iterations,
                timeout: step_timeout,
            };
            let step_start = std::time::Instant::now();

            self.state_store
                .update(run_id, |s| {
                    s.current_step = Some(step.id.clone());
                    if let Some(ss) = s.step_states.get_mut(&step.id) {
                        ss.status = StepStatus::Running;
                        ss.attempt = iter;
                        ss.started_at = Some(Utc::now());
                    }
                })
                .await?;

            // Retry the executor within this loop iteration per the step's
            // `RetryPolicy` (falling back to the workflow's default, which
            // is itself `max_attempts: 3` — see `RetryPolicy::default()`).
            // Loop-mode steps used to ignore this entirely: `execute_loop`
            // journaled and returned on the very first failure, so a
            // step's configured retries never fired (item #486). Reuse the
            // same in-memory `context` across attempts rather than
            // reloading from `state_store` — a failed attempt's partial
            // progress (e.g. `sdd_loop`'s role-agent reply already
            // obtained before its judge call failed) isn't persisted
            // either way, so keeping it in memory means a retry only
            // redoes the part that actually failed.
            let retry_policy = definition.get_retry_policy(step);
            let max_retry_attempts = retry::effective_max_attempts(
                retry_policy.max_attempts,
                matches!(step.on_failure, FailureAction::RetryIndefinitely),
            );
            let mut backoff = Backoff::from_strategy(&retry_policy.backoff);
            let mut retry_attempt = 1;
            let result = loop {
                let attempt_result =
                    timeout(step_timeout, step.executor.execute(&mut context)).await;
                let is_failure = matches!(
                    attempt_result,
                    Ok(Ok(StepResult::Failure)) | Ok(Err(_)) | Err(_)
                );
                if !is_failure {
                    break attempt_result;
                }
                let retryable = match &attempt_result {
                    Ok(Err(e)) => step.executor.is_retryable(e),
                    Err(_) => true,
                    _ => false,
                };
                if !retryable || retry_attempt >= max_retry_attempts {
                    break attempt_result;
                }
                let delay = backoff
                    .next(self.jitter)
                    .unwrap_or_else(|| std::time::Duration::from_secs(1));
                tracing::warn!(
                    run_id = %run_id, step = %step.id, iter, retry_attempt, ?delay,
                    "Loop iteration step failed, retrying"
                );
                tokio::time::sleep(delay).await;
                retry_attempt += 1;
            };
            let duration_ms = step_start.elapsed().as_millis() as u64;

            match result {
                Ok(Ok(StepResult::Success)) => {
                    executed += 1;
                    let out = context.output.clone();
                    self.state_store
                        .update(run_id, |s| {
                            s.context = context.clone();
                            s.input = out.clone();
                            s.output = Some(out.clone());
                            if let Some(var) = step.output_var.as_deref() {
                                capture_output(&mut s.variables, Some(var), &out);
                            }
                            if let Some(ss) = s.step_states.get_mut(&step.id) {
                                ss.status = StepStatus::Succeeded;
                                ss.completed_at = Some(Utc::now());
                                ss.input_tokens = context.input_tokens;
                                ss.output_tokens = context.output_tokens;
                                ss.duration_ms = duration_ms;
                            }
                        })
                        .await?;
                    self.state_store
                        .append_journal(
                            run_id,
                            JournalEntry::LoopIteration {
                                step_id: step.id.clone(),
                                iteration: iter,
                                output: out.clone().into_bytes(),
                            },
                        )
                        .await?;
                    last_context = Some(context.clone());
                    tracing::info!(run_id = %run_id, step = %step.id, iter, "Loop iteration completed");
                    if !until_lower.is_empty() && out.to_lowercase().contains(&until_lower) {
                        break;
                    }
                }
                Ok(Ok(StepResult::Skip)) => break,
                Ok(Ok(StepResult::Failure)) | Ok(Err(_)) | Err(_) => {
                    let error_msg = match &result {
                        Err(_) => format!("Step timed out after {step_timeout:?}"),
                        Ok(Err(e)) => format!("{e}"),
                        _ => "Step failed".to_string(),
                    };
                    self.state_store
                        .update(run_id, |s| {
                            if let Some(ss) = s.step_states.get_mut(&step.id) {
                                ss.status = StepStatus::Failed;
                                ss.last_error = Some(error_msg.clone());
                                ss.completed_at = Some(Utc::now());
                            }
                        })
                        .await?;
                    self.state_store
                        .append_journal(
                            run_id,
                            JournalEntry::StepRun {
                                step_id: step.id.clone(),
                                attempt: iter,
                                result: Some(EntryResult::Failure {
                                    code: 1,
                                    message: error_msg,
                                    metadata: vec![],
                                }),
                            },
                        )
                        .await?;
                    return Ok(StepResult::Failure);
                }
            }
        }

        // Journal the loop's terminal success carrying the full serialized
        // context (matching Sequential/FanOut's terminal `StepRun` entry) so
        // a registered rollback handler can reconstruct this step's own
        // output from the journal, same as any other supported mode.
        let final_context = match last_context {
            Some(ctx) => ctx,
            None => self.state_store.load(run_id).await?.context,
        };
        let context_bytes = serde_json::to_vec(&final_context)
            .map_err(|e| WorkflowError::Journal(format!("serialize context: {e}")))?;
        self.state_store
            .append_journal(
                run_id,
                JournalEntry::StepRun {
                    step_id: step.id.clone(),
                    attempt: executed,
                    result: Some(EntryResult::Success(context_bytes)),
                },
            )
            .await?;
        Ok(StepResult::Success)
    }
}
