use super::*;

impl<D: WorkflowData, S: StateStore<D> + 'static> WorkflowEngine<D, S> {
    pub(super) async fn execute_with_budget_deadline(
        &self,
        run_id: WorkflowRunId,
        definition: Arc<WorkflowDefinition<D>>,
    ) -> WorkflowResult<()> {
        let state = self.state_store.load(run_id).await?;
        let deadline = state.budget.as_ref().and_then(|budget| budget.deadline());
        let execution = self.execute_workflow(run_id, definition);
        let Some(deadline) = deadline else {
            return execution.await;
        };
        let remaining = (deadline - chrono::Utc::now()).to_std().unwrap_or_default();
        if remaining.is_zero() {
            return self
                .cancel_workflow_impl(
                    run_id,
                    false,
                    Some(crate::budget::error("elapsed allowance exhausted").to_string()),
                )
                .await;
        }
        match tokio::time::timeout(remaining, execution).await {
            Ok(result) => result,
            Err(_) => {
                self.cancel_workflow_impl(
                    run_id,
                    false,
                    Some(crate::budget::error("elapsed allowance exhausted").to_string()),
                )
                .await
            }
        }
    }
    /// Execute a step with retry logic, appending the terminal result to the
    /// durable journal so recovery never re-executes completed steps.
    pub(super) async fn execute_step_with_retry(
        &self,
        run_id: WorkflowRunId,
        step: &StepDefinition<D>,
        definition: &WorkflowDefinition<D>,
    ) -> WorkflowResult<StepResult> {
        let retry_policy = definition.get_retry_policy(step);
        let step_timeout = definition.get_timeout(step);

        let mut attempt = 1;
        let max_attempts = retry::effective_max_attempts(
            retry_policy.max_attempts,
            matches!(step.on_failure, FailureAction::RetryIndefinitely),
        );
        let mut backoff = Backoff::from_strategy(&retry_policy.backoff);

        loop {
            if self.state_store.is_cancelled(run_id).await? {
                return Err(WorkflowError::Cancelled(run_id));
            }
            if self.should_stop(run_id).await? {
                return Err(WorkflowError::Paused(run_id));
            }

            self.state_store
                .update(run_id, |s| {
                    s.current_step = Some(step.id.clone());
                    if let Some(ss) = s.step_states.get_mut(&step.id) {
                        ss.status = if attempt == 1 {
                            StepStatus::Running
                        } else {
                            StepStatus::Retrying
                        };
                        ss.attempt = attempt;
                        ss.started_at = Some(Utc::now());
                    }
                })
                .await?;

            self.event_bus
                .publish(WorkflowEvent::StepStarted {
                    run_id,
                    step_id: step.id.clone(),
                    attempt,
                })
                .await;

            let state = self.state_store.load(run_id).await?;
            let mut context = state.context.clone();
            let step_timeout = state
                .budget
                .as_ref()
                .map(|budget| budget.cap_timeout(step_timeout))
                .transpose()?
                .unwrap_or(step_timeout);
            context.budget = state.budget.as_ref().map(|_| {
                crate::budget::BudgetHandle::new(
                    self.state_store.clone(),
                    run_id,
                    step.id.to_string(),
                    attempt,
                )
            });
            self.apply_data_patch(run_id, &mut context.data);
            context.input = state.input.clone();
            context.variables = state.variables.clone();
            context.step = StepExecutionMeta {
                workflow_id: definition.id.to_string(),
                workflow_name: definition.name.clone(),
                step_id: step.id.to_string(),
                step_name: step.name.clone(),
                attempt,
                max_attempts,
                timeout: step_timeout,
            };
            let step_start = std::time::Instant::now();
            let result = timeout(step_timeout, step.executor.execute(&mut context)).await;
            let step_duration = step_start.elapsed();
            // Superseded mid-step: the new holder owns the run now, so record
            // nothing (the step re-runs there) and stop as for a pause.
            if self.is_superseded(run_id) {
                return Err(WorkflowError::Paused(run_id));
            }

            if !matches!(result, Ok(Ok(StepResult::Skip))) {
                self.state_store
                    .update(run_id, |s| {
                        s.context = context.clone();
                        // A patch registered while this step was executing
                        // must survive the step's stale write-back.
                        self.apply_data_patch(run_id, &mut s.context.data);
                    })
                    .await?;
            }

            let context_bytes = serde_json::to_vec(&context)
                .map_err(|e| WorkflowError::Journal(format!("serialize context: {e}")))?;

            match result {
                Ok(Ok(StepResult::Success)) => {
                    self.state_store
                        .update(run_id, |s| {
                            // Chain the string pipeline: step output -> {{input}}.
                            let out = context.output.clone();
                            s.input = out.clone();
                            s.output = Some(out.clone());
                            if let Some(var) = step.output_var.as_deref() {
                                capture_output(&mut s.variables, Some(var), &out);
                                // Keep the persisted context's variables (what
                                // `run_if` reads via `get_context`) in sync
                                // with this step's own just-captured output —
                                // otherwise a downstream step's run_if sees a
                                // one-step-stale snapshot.
                                s.context.variables = s.variables.clone();
                            }
                            if let Some(ss) = s.step_states.get_mut(&step.id) {
                                ss.status = StepStatus::Succeeded;
                                ss.completed_at = Some(Utc::now());
                                ss.input_tokens = context.input_tokens;
                                ss.output_tokens = context.output_tokens;
                                ss.duration_ms = step_duration.as_millis() as u64;
                            }
                        })
                        .await?;
                    self.state_store
                        .append_journal(
                            run_id,
                            JournalEntry::StepRun {
                                step_id: step.id.clone(),
                                attempt,
                                result: Some(EntryResult::Success(context_bytes)),
                            },
                        )
                        .await?;
                    self.event_bus
                        .publish(WorkflowEvent::StepSucceeded {
                            run_id,
                            step_id: step.id.clone(),
                            duration: step_duration,
                        })
                        .await;
                    if let Err(e) = step.executor.on_success(&context).await {
                        tracing::warn!(step_id = %step.id, error = ?e, "on_success hook failed");
                    }
                    return Ok(StepResult::Success);
                }
                Ok(Ok(StepResult::Skip)) => {
                    return Ok(StepResult::Skip);
                }
                Ok(Ok(StepResult::Failure))
                | Ok(Ok(StepResult::Failed(_)))
                | Ok(Err(_))
                | Err(_) => {
                    // The failure is the pause killing this step's work (an
                    // agent turn stopped via its job's cancel flag): not a
                    // real failure, so nothing is retried or journaled.
                    if self.state_store.is_paused(run_id).await? {
                        self.mark_step_paused(run_id, &step.id).await?;
                        return Err(WorkflowError::Paused(run_id));
                    }
                    let (error_msg, should_retry) = match result {
                        Ok(Err(e)) => {
                            let retryable = step.executor.is_retryable(&e);
                            (format!("{e}"), retryable)
                        }
                        Err(_) => (format!("Step timeout after {step_timeout:?}"), true),
                        // Carries its own reason — never retried, same as `Failure`.
                        Ok(Ok(StepResult::Failed(ref msg))) => (msg.clone(), false),
                        _ => ("Step failed".to_string(), false),
                    };

                    let will_retry = should_retry && attempt < max_attempts;

                    self.state_store
                        .update(run_id, |s| {
                            if let Some(ss) = s.step_states.get_mut(&step.id) {
                                ss.status = if will_retry {
                                    StepStatus::Retrying
                                } else {
                                    StepStatus::Failed
                                };
                                ss.last_error = Some(error_msg.clone());
                                if !will_retry {
                                    ss.completed_at = Some(Utc::now());
                                }
                            }
                        })
                        .await?;

                    self.event_bus
                        .publish(WorkflowEvent::StepFailed {
                            run_id,
                            step_id: step.id.clone(),
                            error: error_msg.clone(),
                            will_retry,
                        })
                        .await;

                    if will_retry {
                        let delay = backoff
                            .next(self.jitter)
                            .unwrap_or_else(|| Duration::from_secs(1));
                        self.event_bus
                            .publish(WorkflowEvent::StepRetrying {
                                run_id,
                                step_id: step.id.clone(),
                                attempt: attempt + 1,
                                delay,
                            })
                            .await;
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    } else {
                        let hook_error = WorkflowError::StepFailed {
                            step_id: step.id.clone(),
                            message: error_msg.clone(),
                        };
                        if let Err(e) = step.executor.on_failure(&context, &hook_error).await {
                            tracing::warn!(step_id = %step.id, error = ?e, "on_failure hook failed");
                        }
                        // Terminal failure is journaled so recovery sees the
                        // step as failed and does not re-run it from scratch.
                        self.state_store
                            .append_journal(
                                run_id,
                                JournalEntry::StepRun {
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
                        // ErrorMode::Skip (and FailureAction::ContinueNextStep)
                        // turn a terminal failure into a skip so the workflow
                        // continues.
                        let skip = matches!(step.error_mode, ErrorMode::Skip)
                            || matches!(step.on_failure, FailureAction::ContinueNextStep);
                        return Ok(if skip {
                            StepResult::Skip
                        } else {
                            StepResult::Failure
                        });
                    }
                }
            }
        }
    }
}
