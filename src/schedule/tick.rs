//! Daemon-owned scheduling: one tick at a time — reconcile outstanding
//! dispatching/dispatched occurrences, claim due slots, claim the queue and
//! spawn daemon-owned fire tasks, prune touched histories. SQLite sections
//! are blocking and transactional; engine calls are async on the ambient
//! runtime; never a transaction across a filesystem read, an async wait, or
//! an agent launch.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use flare_workflow::json::{JsonWorkflow, PipelineData, SendMessage, compile_workflow_in};
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::types::WorkflowStatus;
use flare_workflow::{OrphanCancelDisposition, StateStore, WorkflowEngine, WorkflowRunId};

use super::store::SlotOutcome;
use super::{OccurrenceRow, START_RECOVERY_DELAY_SECS, Scheduler, TickSummary, start_key_for};

/// RAII in-flight tracking: a live attempt is never timed out into another
/// attempt or cancelled as an orphan merely because its initial lease aged.
struct InFlightGuard {
    set: Arc<Mutex<HashSet<String>>>,
    id: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.set.lock().expect("in-flight set").remove(&self.id);
    }
}

fn terminal_str(status: WorkflowStatus) -> &'static str {
    match status {
        WorkflowStatus::Completed => "completed",
        WorkflowStatus::Failed => "failed",
        WorkflowStatus::Cancelled => "cancelled",
        _ => "interrupted",
    }
}

impl Scheduler {
    /// One scheduler pass (§6.1 ordering). Database unavailability fails the
    /// tick; one bad row never aborts the rest (recorded in `errors`).
    pub async fn tick_once(&self, now: DateTime<Utc>) -> Result<TickSummary, String> {
        let _tick = self.tick_lock.lock().await;
        self.reap_fires();
        let mut summary = TickSummary::default();
        let mut touched: HashSet<String> = HashSet::new();

        for occ in self.store.list_open("dispatching").await? {
            touched.insert(occ.schedule_id.clone());
            match self.reconcile_dispatching(&occ, now).await {
                Ok(true) => summary.reconciled += 1,
                Ok(false) => {}
                Err(e) => summary
                    .errors
                    .push(format!("reconcile dispatching {}: {e}", occ.id)),
            }
        }
        for occ in self.store.list_open("dispatched").await? {
            touched.insert(occ.schedule_id.clone());
            match self.reconcile_dispatched(&occ, now).await {
                Ok(true) => summary.reconciled += 1,
                Ok(false) => {}
                Err(e) => summary
                    .errors
                    .push(format!("reconcile dispatched {}: {e}", occ.id)),
            }
        }
        for sched in self.store.due_schedules(now.timestamp()).await? {
            touched.insert(sched.id.clone());
            match self.store.claim_due_slot(&sched.id, now.timestamp()).await {
                Ok(SlotOutcome::Admitted { .. }) => summary.admitted += 1,
                Ok(SlotOutcome::SkippedMissed { .. }) => summary.skipped_missed += 1,
                Ok(SlotOutcome::SkippedOverlap { .. }) => summary.skipped_overlap += 1,
                Ok(SlotOutcome::Frozen) => summary
                    .errors
                    .push(format!("schedule '{}' frozen, skipped", sched.name)),
                Ok(SlotOutcome::NotDue) => {}
                Err(e) => summary
                    .errors
                    .push(format!("claim slot for '{}': {e}", sched.name)),
            }
        }
        match self.store.claim_queued(now.timestamp()).await {
            Ok(claimed) => {
                for occ in claimed {
                    touched.insert(occ.schedule_id.clone());
                    self.spawn_fire(occ.id.clone()).await;
                    summary.fired += 1;
                }
            }
            Err(e) => summary.errors.push(format!("claim queue: {e}")),
        }
        for sid in touched {
            match self.store.prune_schedule(&sid).await {
                Ok(n) => summary.pruned += n,
                Err(e) => summary.errors.push(format!("prune {sid}: {e}")),
            }
        }
        Ok(summary)
    }

    /// Unlinked dispatch recovery (§6.4): a live in-flight attempt is left
    /// alone; a receipt links without a retry; otherwise the delay decides
    /// between waiting, requeueing, and `dispatch_failed`.
    pub(super) async fn reconcile_dispatching(
        &self,
        occ: &OccurrenceRow,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        if self.is_in_flight(&occ.id) {
            return Ok(false);
        }
        let key = start_key_for(&occ.id);
        match self.lookup_start(&key).await? {
            Some(run_id) => {
                self.store.link_run(&occ.id, &run_id.to_string()).await?;
                self.reconcile_linked(&occ.id, run_id, now).await
            }
            None => {
                let started = occ.dispatch_started_at.unwrap_or(occ.created_at);
                if now.timestamp() - started < START_RECOVERY_DELAY_SECS {
                    return Ok(false);
                }
                if occ.trigger == "manual" {
                    // Manuals never expire.
                    return self.store.mark_queued(&occ.id).await;
                }
                match occ.start_deadline_at {
                    // Same occurrence, same slot/spec/key — only ever retried
                    // within its scheduled deadline.
                    Some(d) if now.timestamp() <= d => Ok(self.store.mark_queued(&occ.id).await?),
                    Some(_) => Ok(self
                        .store
                        .mark_dispatch_failed(
                            &occ.id,
                            "lost before start; outside grace",
                            now.timestamp(),
                        )
                        .await?),
                    // Defensive: a scheduled row without a deadline must not
                    // lose work silently; requeue it.
                    None => Ok(self.store.mark_queued(&occ.id).await?),
                }
            }
        }
    }

    pub(super) async fn reconcile_dispatched(
        &self,
        occ: &OccurrenceRow,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        let Some(run_id) = occ.run_id.as_deref() else {
            return Err(format!("dispatched occurrence {} has no run_id", occ.id));
        };
        let run_id = run_id
            .parse::<WorkflowRunId>()
            .map_err(|e| format!("bad run_id on {}: {e}", occ.id))?;
        self.reconcile_linked(&occ.id, run_id, now).await
    }

    /// Linked-run reconciliation + tick fallback (§6.5): copy real terminal
    /// outcomes, mark evicted rows, leave paused and fresh-lease runs alone,
    /// and conditionally cancel only provably-stale orphans.
    pub(super) async fn reconcile_linked(
        &self,
        occurrence_id: &str,
        run_id: WorkflowRunId,
        now: DateTime<Utc>,
    ) -> Result<bool, String> {
        reconcile_linked_run(&self.store, &self.engine, occurrence_id, run_id, now).await
    }

    async fn spawn_fire(&self, occurrence_id: String) {
        // Resolve the owning schedule for its register-plus-start lock;
        // the fire task re-reads the row and stops if it moved on.
        let sched_id = match self.store.get_occurrence(&occurrence_id).await {
            Ok(Some(occ)) if occ.status == "dispatching" => occ.schedule_id,
            // Row moved on (linked, settled, requeued, or removed) between
            // claim and spawn: nothing to fire.
            _ => return,
        };
        if !self.mark_in_flight(&occurrence_id) {
            return;
        }
        let store = self.store.clone();
        let engine = self.engine.clone();
        let send = self.send.clone();
        let in_flight = Arc::clone(&self.in_flight);
        let sched_lock = self.schedule_lock(&sched_id);
        let handle = tokio::spawn(async move {
            let _guard = InFlightGuard {
                set: in_flight,
                id: occurrence_id.clone(),
            };
            fire_one(&store, &engine, &send, &sched_lock, occurrence_id).await;
        });
        self.fires.lock().expect("fire handles").push(handle);
    }

    async fn lookup_start(&self, key: &str) -> Result<Option<WorkflowRunId>, String> {
        self.engine
            .state_store()
            .lookup_start(key)
            .await
            .map_err(|e| e.to_string())
    }
}

/// Shared linked-run reconciliation behind `Scheduler::reconcile_linked`
/// and the fire path: after every linkage, converge an already-terminal run
/// at once. This closes the race where a fast driver settles (and its eager
/// write finds the row still `dispatching`) before the fire task's link
/// lands — without it the occurrence would wait for the next tick.
async fn reconcile_linked_run(
    store: &super::ScheduleStore,
    engine: &WorkflowEngine<PipelineData, SqliteStore<PipelineData>>,
    occurrence_id: &str,
    run_id: WorkflowRunId,
    now: DateTime<Utc>,
) -> Result<bool, String> {
    let now_ts = now.timestamp();
    let linked = match engine.state_store().load(run_id).await {
        Ok(s) => Some(s),
        Err(flare_workflow::WorkflowError::NotFound(_)) => None,
        Err(e) => return Err(e.to_string()),
    };
    match linked {
        None => Ok(store
            .settle_open(occurrence_id, "evicted", None, now_ts)
            .await?),
        Some(state) => {
            if state.status.is_terminal() {
                return store
                    .settle_open(
                        occurrence_id,
                        terminal_str(state.status),
                        state.error.as_deref(),
                        now_ts,
                    )
                    .await;
            }
            if state.status == WorkflowStatus::Paused {
                // Intentional inactivity, not a crash orphan — regardless
                // of lease. Still blocks overlap.
                return Ok(false);
            }
            let fresh = state
                .lease
                .as_ref()
                .is_some_and(|l| l.is_fresh(now, engine.lease_ttl()));
            if fresh {
                return Ok(false);
            }
            match engine.cancel_if_orphaned(run_id, now).await {
                Ok(OrphanCancelDisposition::Interrupted) => Ok(store
                    .settle_open(occurrence_id, "interrupted", None, now_ts)
                    .await?),
                Ok(OrphanCancelDisposition::Terminal(status)) => {
                    // Rechecked state settled concurrently; copy the real
                    // outcome — newer state always wins.
                    let current = match engine.state_store().load(run_id).await {
                        Ok(s) if s.status == status => Some(s),
                        _ => None,
                    };
                    Ok(store
                        .settle_open(
                            occurrence_id,
                            terminal_str(status),
                            current.as_ref().and_then(|s| s.error.as_deref()),
                            now_ts,
                        )
                        .await?)
                }
                Ok(OrphanCancelDisposition::Missing) => Ok(store
                    .settle_open(occurrence_id, "evicted", None, now_ts)
                    .await?),
                Ok(OrphanCancelDisposition::Unchanged) => Ok(false),
                Err(e) => Err(format!("cancel_if_orphaned: {e}")),
            }
        }
    }
}

/// Daemon-owned fire (§6.3): receipt recheck, execution-time revalidation,
/// per-schedule register-plus-start serialization, idempotent linkage. Store
/// failures and uncertain starts leave `dispatching` for reconciliation;
/// `dispatch_failed` requires proving no receipt exists.
async fn fire_one(
    store: &super::ScheduleStore,
    engine: &WorkflowEngine<PipelineData, SqliteStore<PipelineData>>,
    send: &SendMessage,
    sched_lock: &Arc<tokio::sync::Mutex<()>>,
    occurrence_id: String,
) {
    let fail = |msg: String| {
        let (store, occurrence_id) = (store.clone(), occurrence_id.clone());
        async move {
            if let Err(e) = store
                .mark_dispatch_failed(&occurrence_id, &msg, Utc::now().timestamp())
                .await
            {
                eprintln!("agentflare-schedule: mark dispatch_failed failed: {e}");
            }
        }
    };
    let outcome = async {
        let occ = store
            .get_occurrence(&occurrence_id)
            .await?
            .ok_or_else(|| "occurrence gone".to_string())?;
        if occ.status != "dispatching" {
            return Ok::<_, String>(());
        }
        let key = start_key_for(&occurrence_id);
        // Before executing, check the receipt. Never restart.
        if let Some(run_id) = engine
            .state_store()
            .lookup_start(&key)
            .await
            .map_err(|e| e.to_string())?
        {
            store.link_run(&occ.id, &run_id.to_string()).await?;
            let _ = reconcile_linked_run(store, engine, &occ.id, run_id, Utc::now()).await;
            return Ok(());
        }
        let spec = occ.parsed_launch_spec()?;
        if spec.params.as_object().is_none() {
            store
                .mark_dispatch_failed(
                    &occ.id,
                    "launch_spec params not an object",
                    Utc::now().timestamp(),
                )
                .await?;
            return Ok(());
        }
        // Execution-time revalidation: current file contents, every fire.
        let definition_json = crate::workflow::resolve_named_definition(
            std::path::Path::new(&spec.repo_root),
            &spec.workflow_name,
        )
        .map_err(|e| format!("definition: {e}"))?;
        let json: JsonWorkflow =
            serde_json::from_str(&definition_json).map_err(|e| format!("definition: {e}"))?;
        let repo_root = std::path::PathBuf::from(&spec.repo_root);
        let mut definition = compile_workflow_in(&json, send.clone(), Some(repo_root))
            .map_err(|e| format!("definition: {e}"))?;
        let definition_id =
            flare_workflow::WorkflowId::new(format!("schedule:{}", occ.schedule_id));
        definition.id = definition_id.clone();
        let mut params = spec.params.clone();
        let params_obj = params
            .as_object_mut()
            .ok_or_else(|| "launch_spec params claim reserved key `schedule`".to_string())?;
        if params_obj.contains_key("schedule") {
            return Err("launch_spec params claim reserved key `schedule`".to_string());
        }
        params_obj.insert(
            "schedule".to_string(),
            serde_json::json!({
                "schedule_id": occ.schedule_id,
                "name": spec.schedule_name,
                "occurrence_id": occ.id,
                "slot": occ.slot,
                "trigger": occ.trigger,
            }),
        );
        // Register-plus-start serialized per schedule; the lock releases
        // after start returns, not after workflow completion.
        let started = {
            let _held = sched_lock.lock().await;
            engine
                .register_workflow(definition)
                .map_err(|e| format!("definition: {e}"))?;
            engine
                .start_workflow_once_with_params(
                    &key,
                    definition_id,
                    PipelineData,
                    String::new(),
                    params,
                )
                .await
                .map_err(|e| e.to_string())?
        };
        store
            .link_run(&occ.id, &started.run_id().to_string())
            .await?;
        // Converge a run that settled between start-return and link-commit
        // (its eager write could not match the still-`dispatching` row).
        let _ = reconcile_linked_run(store, engine, &occ.id, started.run_id(), Utc::now()).await;
        Ok::<_, String>(())
    };
    match outcome.await {
        Ok(()) => {}
        Err(e) if e == "occurrence gone" => {}
        Err(e) if e.starts_with("definition:") || e.contains("launch_spec") => {
            fail(e).await;
        }
        Err(e) => {
            // Uncertain (store failure, shutdown, post-commit ambiguity):
            // prove no receipt exists before calling it a failure.
            match engine
                .state_store()
                .lookup_start(&start_key_for(&occurrence_id))
                .await
            {
                Ok(Some(run_id)) => {
                    let _ = store.link_run(&occurrence_id, &run_id.to_string()).await;
                }
                Ok(None) => {
                    eprintln!(
                        "agentflare-schedule: fire {occurrence_id} uncertain, left dispatching: {e}"
                    );
                }
                Err(le) => {
                    eprintln!(
                        "agentflare-schedule: fire {occurrence_id} uncertain and receipt unreadable ({le}); left dispatching: {e}"
                    );
                }
            }
        }
    }
}
