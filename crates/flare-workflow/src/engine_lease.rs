//! Executor leases, crash recovery and run takeover for [`WorkflowEngine`]:
//! which process is driving a run, renewing/releasing that claim, and
//! resuming non-terminal runs whose holder died. Child module of `engine`
//! (split out for size) so it can reach the engine's private fields.

use std::sync::{Arc, atomic::Ordering};
use std::time::Duration;

use chrono::Utc;

use super::{AbortOnDrop, ActiveWorkflowGuard, DataPatch, DrivingGuard, WorkflowEngine};
use crate::definition::WorkflowDefinition;
use crate::store::StateStore;
use crate::types::*;

/// Default executor-lease TTL: a run whose lease hasn't been renewed for this
/// long is treated as abandoned by a dead process and may be taken over.
pub const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(60);

/// This process's executor identity, `host:pid:nonce`. The random nonce makes
/// it unique even if the OS later reuses the pid (after a reboot, or on a
/// busy host), so a stale lease can never be mistaken for a live one of ours.
pub(super) fn process_executor_id() -> Arc<str> {
    static ID: std::sync::LazyLock<Arc<str>> = std::sync::LazyLock::new(|| {
        let host = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "localhost".to_string());
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        Arc::from(format!("{host}:{}:{nonce}", std::process::id()))
    });
    Arc::clone(&ID)
}

/// Whether a process with `pid` currently exists on this host.
pub(super) fn pid_exists(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        flare_process::command("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        flare_process::command("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

impl<D: WorkflowData, S: StateStore<D> + 'static> WorkflowEngine<D, S> {
    /// Override the executor-lease TTL (default [`DEFAULT_LEASE_TTL`]).
    pub fn with_lease_ttl(mut self, ttl: Duration) -> Self {
        self.lease_ttl = ttl.max(Duration::from_millis(100));
        self
    }

    /// Override this engine's executor identity. Every engine in one process
    /// shares the process identity by default; tests use this to stand in for
    /// a second, independent process sharing the same store.
    pub fn with_executor_id(mut self, id: impl Into<String>) -> Self {
        self.executor_id = Arc::from(id.into());
        self
    }

    /// The identity this engine stamps into the leases of runs it drives.
    pub fn executor_id(&self) -> &str {
        &self.executor_id
    }

    pub fn lease_ttl(&self) -> Duration {
        self.lease_ttl
    }

    /// Whether this engine instance is executing `run_id` right now.
    pub fn is_driving(&self, run_id: WorkflowRunId) -> bool {
        self.driving.lock().contains(&run_id)
    }

    /// Whether `state` is held by a *different*, still-live executor (a
    /// fresh lease stamped by someone other than this engine), i.e. whether
    /// resuming it here would double-run it.
    pub fn is_leased_elsewhere(&self, state: &WorkflowState<D>) -> bool {
        state.lease.as_ref().is_some_and(|lease| {
            *lease.owner != *self.executor_id && lease.is_fresh(Utc::now(), self.lease_ttl)
        })
    }

    /// Whether `lease`'s holder is provably dead: stamped by a different
    /// process on this same host whose pid no longer exists. Lets boot-time
    /// recovery take over a crashed predecessor's runs at once instead of
    /// waiting out the TTL on their last renewal. Anything unprovable (other
    /// host, unparsable owner, pid still present) counts as alive.
    pub(super) fn lease_holder_is_dead(&self, lease: &ExecutorLease) -> bool {
        let mut mine = self.executor_id.rsplitn(3, ':');
        let mut theirs = lease.owner.rsplitn(3, ':');
        let (Some(_), Some(my_pid), Some(my_host)) = (mine.next(), mine.next(), mine.next()) else {
            return false;
        };
        let (Some(_), Some(pid), Some(host)) = (theirs.next(), theirs.next(), theirs.next()) else {
            return false;
        };
        if host != my_host || pid == my_pid {
            return false;
        }
        pid.parse::<u32>().is_ok_and(|pid| !pid_exists(pid))
    }

    /// [`is_leased_elsewhere`](Self::is_leased_elsewhere), minus leases whose
    /// holder is provably dead (see `lease_holder_is_dead`).
    pub(super) fn held_by_live_executor(&self, state: &WorkflowState<D>) -> bool {
        self.is_leased_elsewhere(state)
            && !state
                .lease
                .as_ref()
                .is_some_and(|lease| self.lease_holder_is_dead(lease))
    }

    pub(super) fn new_lease(&self) -> ExecutorLease {
        ExecutorLease {
            owner: self.executor_id.to_string(),
            renewed_at: Utc::now(),
        }
    }

    /// Rebind part of a run's typed data out of band — e.g. hand an
    /// in-flight run to a new claim owner after a restart. The patch is
    /// applied to the persisted context immediately AND re-applied, for as
    /// long as this engine keeps driving the run, to every context a step
    /// loads and every context a step writes back — so a step that was
    /// already executing when the patch landed can't clobber it with its
    /// stale copy on completion. Replaces any earlier patch for the run.
    pub async fn patch_run_data(
        &self,
        run_id: WorkflowRunId,
        patch: impl Fn(&mut D) + Send + Sync + 'static,
    ) -> WorkflowResult<()> {
        let patch: DataPatch<D> = Arc::new(patch);
        self.data_patches.lock().insert(run_id, Arc::clone(&patch));
        self.state_store
            .update(run_id, |s| patch(&mut s.context.data))
            .await
    }

    /// Apply the registered data patch for `run_id` (if any) to `data`.
    pub(crate) fn apply_data_patch(&self, run_id: WorkflowRunId, data: &mut D) {
        if let Some(patch) = self.data_patches.lock().get(&run_id) {
            patch(data);
        }
    }

    /// Drive `run_id` to completion on a spawned task, renewing its executor
    /// lease for as long as the task lives and releasing it afterwards. The
    /// caller must already have inserted `run_id` into `driving` and counted
    /// it in `active_workflows` (`guard` undoes the latter).
    pub(super) fn spawn_driven(
        &self,
        run_id: WorkflowRunId,
        definition: Arc<WorkflowDefinition<D>>,
        guard: ActiveWorkflowGuard,
    ) {
        let engine = self.clone_for_execution();
        let driving = DrivingGuard {
            run_id,
            driving: Arc::clone(&self.driving),
            data_patches: Arc::clone(&self.data_patches),
        };
        self.spawn(async move {
            let _guard = guard;
            let _driving = driving;
            let renewer =
                AbortOnDrop(engine.spawn(engine.clone_for_execution().renew_lease_loop(run_id)));
            let result = engine.execute_workflow(run_id, definition).await;
            drop(renewer);
            engine.release_lease(run_id).await;
            if let Err(e) = result {
                tracing::error!(run_id = %run_id, error = ?e, "Workflow execution failed");
            }
        });
    }

    /// Re-stamp this engine's lease on `run_id` every quarter-TTL. Stops if
    /// another live executor has taken the run over meanwhile (never steals a
    /// fresh lease back) or the run disappears.
    pub(super) async fn renew_lease_loop(self, run_id: WorkflowRunId) {
        let interval = self.lease_ttl / 4;
        loop {
            tokio::time::sleep(interval).await;
            let mut lost_to: Option<String> = None;
            let result = self
                .state_store
                .update(run_id, |s| {
                    if self.is_leased_elsewhere(s) {
                        lost_to = s.lease.as_ref().map(|l| l.owner.clone());
                    } else {
                        s.lease = Some(self.new_lease());
                    }
                })
                .await;
            match result {
                Ok(()) => {
                    if let Some(other) = lost_to {
                        tracing::warn!(run_id = %run_id, holder = %other, "Executor lease taken over by another process; no longer renewing it");
                        return;
                    }
                }
                Err(WorkflowError::NotFound(_)) => return,
                Err(e) => {
                    tracing::warn!(run_id = %run_id, error = ?e, "Failed to renew executor lease");
                }
            }
        }
    }

    /// Clear this engine's lease on `run_id` so a non-terminal leftover can
    /// be taken over immediately instead of after the TTL. Best-effort.
    pub(super) async fn release_lease(&self, run_id: WorkflowRunId) {
        let _ = self
            .state_store
            .update(run_id, |s| {
                if s.lease
                    .as_ref()
                    .is_some_and(|l| *l.owner == *self.executor_id)
                {
                    s.lease = None;
                }
            })
            .await;
    }

    /// Resume every `Running`/`Pending`/`Waiting` run found in the store
    /// after a crash or restart. Recovery replays each run's journal, skips
    /// steps that already completed (memoized), and re-drives the pending
    /// remainder. Returns the set of resumed run IDs.
    ///
    /// A run whose executor lease is still fresh and held by another process
    /// (e.g. a live `agentflare work` CLI driving it) is left alone; one whose
    /// lease went stale (its holder died) is taken over. One bad row never
    /// aborts the sweep: its error is logged and the rest still resume.
    pub async fn recover(&self) -> WorkflowResult<Vec<WorkflowRunId>> {
        self.recover_with(|_| true).await
    }

    /// [`recover`](Self::recover), but only resumes runs for which `filter`
    /// returns `true` — the caller's chance to veto a run using state the
    /// generic engine can't see (e.g. its work item was completed or
    /// cancelled out of band while the process was down). Vetoed runs are
    /// left untouched.
    pub async fn recover_with<F>(&self, filter: F) -> WorkflowResult<Vec<WorkflowRunId>>
    where
        F: Fn(&WorkflowState<D>) -> bool,
    {
        let active = self.state_store.list_active().await?;
        let mut resumed = Vec::new();
        for state in active {
            let run_id = state.run_id;
            let definition = match self.definitions.read().get(&state.workflow_id).cloned() {
                Some(def) => def,
                None => {
                    tracing::warn!(run_id = %run_id, workflow_id = %state.workflow_id, "Cannot recover run: definition not registered");
                    continue;
                }
            };
            if !filter(&state) {
                tracing::info!(run_id = %run_id, "Recovery filter vetoed resuming run");
                continue;
            }
            match self.try_resume(&state, definition).await {
                Ok(true) => resumed.push(run_id),
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(run_id = %run_id, error = ?e, "Failed to recover run; skipping it");
                }
            }
        }
        Ok(resumed)
    }

    /// Take over and resume one non-terminal run, unless this engine is
    /// already driving it or another live process holds its lease. Returns
    /// whether it was resumed here. Used by a caller that finds an existing
    /// run it would otherwise just wait on — if the process that was
    /// executing it died, waiting would never end.
    pub async fn resume_run(&self, run_id: WorkflowRunId) -> WorkflowResult<bool> {
        let state = self.state_store.load(run_id).await?;
        if matches!(
            state.status,
            WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
        ) {
            return Ok(false);
        }
        let definition = self
            .definitions
            .read()
            .get(&state.workflow_id)
            .cloned()
            .ok_or_else(|| WorkflowError::DefinitionNotFound(state.workflow_id.clone()))?;
        self.try_resume(&state, definition).await
    }

    pub(super) async fn try_resume(
        &self,
        state: &WorkflowState<D>,
        definition: Arc<WorkflowDefinition<D>>,
    ) -> WorkflowResult<bool> {
        let run_id = state.run_id;
        if self.held_by_live_executor(state) {
            tracing::info!(run_id = %run_id, holder = ?state.lease.as_ref().map(|l| &l.owner), "Run is held by another live executor; not resuming it here");
            return Ok(false);
        }
        if !self.driving.lock().insert(run_id) {
            return Ok(false);
        }
        // Re-check under the store's update (the listing above may be stale)
        // and claim the lease in the same write.
        let mut taken = true;
        let result = self
            .state_store
            .update(run_id, |s| {
                if self.held_by_live_executor(s)
                    || matches!(
                        s.status,
                        WorkflowStatus::Completed
                            | WorkflowStatus::Failed
                            | WorkflowStatus::Cancelled
                    )
                {
                    taken = false;
                    return;
                }
                s.status = WorkflowStatus::Running;
                s.lease = Some(self.new_lease());
            })
            .await;
        if let Err(e) = result {
            self.driving.lock().remove(&run_id);
            return Err(e);
        }
        if !taken {
            self.driving.lock().remove(&run_id);
            return Ok(false);
        }
        self.active_workflows.fetch_add(1, Ordering::AcqRel);
        self.spawn_driven(run_id, definition, self.active_workflow_guard());
        Ok(true)
    }
}
