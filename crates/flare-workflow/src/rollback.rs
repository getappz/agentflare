//! Saga rollback (compensation): best-effort reverse-order unwind of every
//! succeeded step that registered a `rollback` handler, run once a workflow
//! settles to `WorkflowStatus::Failed`.
//!
//! Design: `ROLLBACK_COMPENSATION_DESIGN.md` (item #118). Mirrors `waits.rs`:
//! durable-wait-shaped methods on `WorkflowEngine<D, S>` in their own module.

use std::collections::HashMap;
use std::time::Duration;

use crate::definition::WorkflowDefinition;
use crate::engine::WorkflowEngine;
use crate::events::WorkflowEvent;
use crate::retry::Backoff;
use crate::store::StateStore;
use crate::types::*;

impl<D: WorkflowData, S: StateStore<D> + 'static> WorkflowEngine<D, S> {
    /// Run the saga unwind for `run_id`: every succeeded step with a
    /// registered `rollback` (plus `failed_step`, if it registered one), in
    /// reverse step-start order. Best-effort per step — a handler that
    /// exhausts its retries never aborts the sweep. Journal-resumable: a
    /// step with an already-completed `Rollback` entry is skipped, so a
    /// crash mid-unwind and re-entry via `recover()` picks up where it left
    /// off (exactly-once compensation).
    pub(crate) async fn run_rollback_phase(
        &self,
        run_id: WorkflowRunId,
        definition: &WorkflowDefinition<D>,
        failed_step: Option<&StepId>,
    ) -> WorkflowResult<()> {
        let state = self.state_store.load(run_id).await?;
        let journal = self.state_store.journal(run_id).await?;

        // Steps already processed by a prior (crashed) rollback phase:
        // `true` = compensated successfully, `false` = exhausted retries.
        let mut already_processed: HashMap<StepId, bool> = HashMap::new();
        for entry in &journal {
            if let JournalEntry::Rollback {
                step_id,
                result: Some(result),
                ..
            } = entry
            {
                already_processed
                    .insert(step_id.clone(), matches!(result, EntryResult::Success(_)));
            }
        }

        let mut eligible: Vec<(usize, &crate::definition::StepDefinition<D>)> = Vec::new();
        for (idx, step) in definition.steps.iter().enumerate() {
            if step.rollback.is_none() {
                continue;
            }
            let is_trigger = failed_step == Some(&step.id);
            let succeeded = state
                .step_states
                .get(&step.id)
                .map(|ss| ss.status == StepStatus::Succeeded)
                .unwrap_or(false);
            if succeeded || is_trigger {
                eligible.push((idx, step));
            }
        }

        // Reverse step-start order: latest `started_at` first, tie-broken by
        // reverse declaration index (deterministic, no new field needed).
        eligible.sort_by(|(idx_a, step_a), (idx_b, step_b)| {
            let started_a = state
                .step_states
                .get(&step_a.id)
                .and_then(|ss| ss.started_at);
            let started_b = state
                .step_states
                .get(&step_b.id)
                .and_then(|ss| ss.started_at);
            started_b.cmp(&started_a).then(idx_b.cmp(idx_a))
        });

        let mut compensated = Vec::new();
        let mut failed = Vec::new();

        for (_, step) in eligible {
            if let Some(&ok) = already_processed.get(&step.id) {
                if ok {
                    compensated.push(step.id.clone());
                } else {
                    failed.push(step.id.clone());
                }
                continue;
            }

            let is_trigger = failed_step == Some(&step.id);
            let context = if is_trigger {
                // Cloudflare's `output: undefined` case: this step has no
                // successful `StepRun` snapshot to reconstruct from — seed
                // from the live context at failure time with output cleared.
                let mut ctx = state.context.clone();
                ctx.output = String::new();
                Some(ctx)
            } else {
                journal.iter().rev().find_map(|e| match e {
                    JournalEntry::StepRun {
                        step_id,
                        result: Some(EntryResult::Success(bytes)),
                        ..
                    } if step_id == &step.id => {
                        serde_json::from_slice::<WorkflowContext<D>>(bytes).ok()
                    }
                    _ => None,
                })
            };

            let Some(mut context) = context else {
                tracing::warn!(
                    run_id = %run_id,
                    step_id = %step.id,
                    "rollback: no journaled context snapshot to reconstruct from, skipping"
                );
                let error = "no journaled context snapshot for rollback".to_string();
                self.state_store
                    .append_journal(
                        run_id,
                        JournalEntry::Rollback {
                            step_id: step.id.clone(),
                            attempt: 0,
                            result: Some(EntryResult::Failure {
                                code: 1,
                                message: error.clone(),
                                metadata: vec![],
                            }),
                        },
                    )
                    .await?;
                self.event_bus
                    .publish(WorkflowEvent::RollbackStepFailed {
                        run_id,
                        step_id: step.id.clone(),
                        error,
                    })
                    .await;
                failed.push(step.id.clone());
                continue;
            };

            let rollback_executor = step
                .rollback
                .as_ref()
                .expect("filtered to rollback.is_some() above");
            let retry_policy = step
                .rollback_retry_policy
                .as_ref()
                .or(step.retry_policy.as_ref())
                .unwrap_or(&definition.default_retry_policy);
            let step_timeout = definition.get_timeout(step);
            let max_attempts = retry_policy.max_attempts;
            let mut backoff = Backoff::from_strategy(&retry_policy.backoff);
            let mut attempt = 1u32;

            let outcome = loop {
                let result =
                    tokio::time::timeout(step_timeout, rollback_executor.execute(&mut context))
                        .await;
                match result {
                    Ok(Ok(StepResult::Success)) | Ok(Ok(StepResult::Skip)) => break Ok(()),
                    Ok(Ok(StepResult::Failure))
                    | Ok(Ok(StepResult::Failed(_)))
                    | Ok(Err(_))
                    | Err(_) => {
                        let (error_msg, should_retry) = match &result {
                            Ok(Ok(StepResult::Failed(msg))) => (msg.clone(), false),
                            Ok(Err(e)) => (format!("{e}"), rollback_executor.is_retryable(e)),
                            Err(_) => (format!("rollback timeout after {step_timeout:?}"), true),
                            _ => ("rollback failed".to_string(), false),
                        };
                        let will_retry = should_retry && attempt < max_attempts;
                        if will_retry {
                            let delay = backoff
                                .next(self.jitter)
                                .unwrap_or_else(|| Duration::from_secs(1));
                            tokio::time::sleep(delay).await;
                            attempt += 1;
                            continue;
                        }
                        break Err(error_msg);
                    }
                }
            };

            match outcome {
                Ok(()) => {
                    self.state_store
                        .append_journal(
                            run_id,
                            JournalEntry::Rollback {
                                step_id: step.id.clone(),
                                attempt,
                                result: Some(EntryResult::Success(context.output.into_bytes())),
                            },
                        )
                        .await?;
                    compensated.push(step.id.clone());
                }
                Err(error_msg) => {
                    tracing::warn!(
                        run_id = %run_id,
                        step_id = %step.id,
                        error = %error_msg,
                        "rollback handler exhausted retries, continuing unwind"
                    );
                    self.state_store
                        .append_journal(
                            run_id,
                            JournalEntry::Rollback {
                                step_id: step.id.clone(),
                                attempt,
                                result: Some(EntryResult::Failure {
                                    code: 1,
                                    message: error_msg.clone(),
                                    metadata: vec![],
                                }),
                            },
                        )
                        .await?;
                    self.event_bus
                        .publish(WorkflowEvent::RollbackStepFailed {
                            run_id,
                            step_id: step.id.clone(),
                            error: error_msg,
                        })
                        .await;
                    failed.push(step.id.clone());
                }
            }
        }

        if !failed.is_empty() {
            let failed_list = failed
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.state_store
                .update(run_id, |s| {
                    let msg = format!("rollback failed for steps: {failed_list}");
                    s.error = Some(match &s.error {
                        Some(existing) => format!("{existing}; {msg}"),
                        None => msg,
                    });
                })
                .await?;
        }

        self.event_bus
            .publish(WorkflowEvent::RollbackCompleted {
                run_id,
                compensated,
                failed,
            })
            .await;

        Ok(())
    }
}
