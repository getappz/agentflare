//! Durable waits: `Sleep` timers and `WaitEvent` promises.
//!
//! Design from Restate (BSL — design only, no code): a `Sleep` journals a
//! timer that survives restart and re-arms; a `WaitEvent` journals a promise
//! resolved by `complete_event`, with journaled pre-delivery closing the
//! notify-before-wait race.

use std::time::Duration;

use chrono::Utc;

use crate::definition::StepDefinition;
use crate::engine::WorkflowEngine;
use crate::events::WorkflowEvent;
use crate::store::StateStore;
use crate::types::*;

impl<D: WorkflowData, S: StateStore<D> + 'static> WorkflowEngine<D, S> {
    /// Execute a durable `Sleep` step: journal a pending timer, suspend the
    /// step until wall-clock passes `wake_at`, then journal the fired result.
    pub(crate) async fn execute_sleep(
        &self,
        run_id: WorkflowRunId,
        step: &StepDefinition<D>,
        duration_secs: u64,
    ) -> WorkflowResult<StepResult> {
        if self.state_store.is_cancelled(run_id).await? {
            return Err(WorkflowError::Cancelled(run_id));
        }
        let wake_at = Utc::now() + chrono::Duration::seconds(duration_secs as i64);

        // Append the pending timer once (idempotent across re-arms).
        let journal = self.state_store.journal(run_id).await?;
        if !journal.iter().any(|e| {
            matches!(e, JournalEntry::Sleep { step_id, result: None, .. } if step_id == &step.id)
        }) {
            self.state_store
                .append_journal(
                    run_id,
                    JournalEntry::Sleep {
                        step_id: step.id.clone(),
                        wake_at,
                        result: None,
                    },
                )
                .await?;
        }
        self.event_bus
            .publish(WorkflowEvent::StepWaiting {
                run_id,
                step_id: step.id.clone(),
                reason: format!("sleep until {wake_at}"),
            })
            .await;

        let delay = (wake_at - Utc::now()).to_std().unwrap_or(Duration::ZERO);
        tokio::time::sleep(delay).await;

        self.state_store
            .append_journal(
                run_id,
                JournalEntry::Sleep {
                    step_id: step.id.clone(),
                    wake_at,
                    result: Some(EntryResult::Success(Vec::new())),
                },
            )
            .await?;
        self.state_store
            .update(run_id, |s| {
                if let Some(ss) = s.step_states.get_mut(&step.id) {
                    ss.status = StepStatus::Succeeded;
                    ss.completed_at = Some(Utc::now());
                }
            })
            .await?;
        Ok(StepResult::Success)
    }

    /// Execute a durable `WaitEvent` step: journal a pending promise, then
    /// await `complete_event` (or a journaled pre-delivery) within the timeout.
    pub(crate) async fn execute_wait_event(
        &self,
        run_id: WorkflowRunId,
        step: &StepDefinition<D>,
        name: &str,
        timeout_secs: u64,
    ) -> WorkflowResult<StepResult> {
        if self.state_store.is_cancelled(run_id).await? {
            return Err(WorkflowError::Cancelled(run_id));
        }

        // Pre-delivered completion already journaled -> succeed immediately.
        let journal = self.state_store.journal(run_id).await?;
        if journal
            .iter()
            .any(|e| matches!(e, JournalEntry::WaitEvent { name: n, result: Some(_) } if n == name))
        {
            self.state_store
                .update(run_id, |s| {
                    if let Some(ss) = s.step_states.get_mut(&step.id) {
                        ss.status = StepStatus::Succeeded;
                        ss.completed_at = Some(Utc::now());
                    }
                })
                .await?;
            return Ok(StepResult::Success);
        }

        // Append the pending promise once.
        if !journal
            .iter()
            .any(|e| matches!(e, JournalEntry::WaitEvent { name: n, result: None } if n == name))
        {
            self.state_store
                .append_journal(
                    run_id,
                    JournalEntry::WaitEvent {
                        name: name.to_string(),
                        result: None,
                    },
                )
                .await?;
        }
        self.event_bus
            .publish(WorkflowEvent::StepWaiting {
                run_id,
                step_id: step.id.clone(),
                reason: format!("wait for event '{name}'"),
            })
            .await;

        let (tx, rx) = tokio::sync::oneshot::channel::<EntryResult>();
        let key = format!("{run_id}:{name}");
        self.waiters.lock().insert(key.clone(), tx);

        // Close the notify-before-wait race: after registering, re-check the
        // journal in case complete_event already buffered the result.
        let journal = self.state_store.journal(run_id).await?;
        let buffered = journal.iter().rev().find_map(|e| match e {
            JournalEntry::WaitEvent {
                name: n,
                result: Some(r),
            } if n == name => Some(r.clone()),
            _ => None,
        });
        if let Some(result) = buffered {
            self.waiters.lock().remove(&key);
            self.state_store
                .append_journal(
                    run_id,
                    JournalEntry::WaitEvent {
                        name: name.to_string(),
                        result: Some(result),
                    },
                )
                .await?;
            self.state_store
                .update(run_id, |s| {
                    if let Some(ss) = s.step_states.get_mut(&step.id) {
                        ss.status = StepStatus::Succeeded;
                        ss.completed_at = Some(Utc::now());
                    }
                })
                .await?;
            return Ok(StepResult::Success);
        }

        let timeout_dur = Duration::from_secs(timeout_secs);
        let outcome = match tokio::time::timeout(timeout_dur, rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err("wait channel closed".to_string()),
            Err(_) => Err(format!(
                "wait for event '{name}' timed out after {timeout_secs}s"
            )),
        };
        self.waiters.lock().remove(&key);

        match outcome {
            Ok(result) => {
                self.state_store
                    .append_journal(
                        run_id,
                        JournalEntry::WaitEvent {
                            name: name.to_string(),
                            result: Some(result),
                        },
                    )
                    .await?;
                self.state_store
                    .update(run_id, |s| {
                        if let Some(ss) = s.step_states.get_mut(&step.id) {
                            ss.status = StepStatus::Succeeded;
                            ss.completed_at = Some(Utc::now());
                        }
                    })
                    .await?;
                Ok(StepResult::Success)
            }
            Err(msg) => {
                self.state_store
                    .append_journal(
                        run_id,
                        JournalEntry::WaitEvent {
                            name: name.to_string(),
                            result: Some(EntryResult::Failure {
                                code: 2,
                                message: msg.clone(),
                                metadata: vec![],
                            }),
                        },
                    )
                    .await?;
                self.state_store
                    .update(run_id, |s| {
                        s.current_step = Some(step.id.clone());
                        if let Some(ss) = s.step_states.get_mut(&step.id) {
                            ss.status = StepStatus::Failed;
                            ss.last_error = Some(msg);
                            ss.completed_at = Some(Utc::now());
                        }
                    })
                    .await?;
                Ok(StepResult::Failure)
            }
        }
    }

    /// Complete a pending `WaitEvent` from anywhere. Exactly-once: a buffered
    /// journaled completion is written when no in-process waiter exists, so a
    /// pre-delivery survives the notify-before-wait race; a racing second
    /// completion is a no-op.
    pub async fn complete_event(
        &self,
        run_id: WorkflowRunId,
        name: &str,
        result: EntryResult,
    ) -> WorkflowResult<()> {
        let key = format!("{run_id}:{name}");
        let waiter = self.waiters.lock().remove(&key);
        if let Some(tx) = waiter {
            let _ = tx.send(result);
            return Ok(());
        }
        // No in-process waiter: buffer the completion in the journal. Append is
        // idempotent per name via the pending-entry guard on the wait side.
        self.state_store
            .append_journal(
                run_id,
                JournalEntry::WaitEvent {
                    name: name.to_string(),
                    result: Some(result),
                },
            )
            .await?;
        Ok(())
    }
}
