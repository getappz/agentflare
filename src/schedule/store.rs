//! Blocking schedule storage: `schedules` CRUD, admission, manual queue,
//! linkage, retention. Every method is `async` but runs its SQLite section on
//! a blocking thread; transactions never span an `.await`, a filesystem
//! read, or an agent launch.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use super::{
    FireReceipt, LaunchSpec, OccurrenceRow, RETENTION_NON_SKIP, RETENTION_SKIP, SKIP_STATUSES,
    ScheduleRow, TERMINAL_NON_SKIP_STATUSES, canonical_repo_root, validate_every_secs,
    validate_grace_secs, validate_name, validate_overlap, validate_params,
};
use flare_workflow::json::{JsonWorkflow, PipelineData, compile_workflow};
use flare_workflow::sqlite_store::SqliteStore;

/// Outcome of one transactional slot claim.
#[derive(Debug, Clone)]
pub enum SlotOutcome {
    Admitted {
        occurrence_id: String,
    },
    SkippedMissed {
        occurrence_id: String,
    },
    SkippedOverlap {
        occurrence_id: String,
    },
    /// Row gone, disabled, frozen, or no longer due (lost race / no-op).
    NotDue,
    /// Malformed row (bad params, overflow): frozen, clock untouched.
    Frozen,
}

/// Inputs for [`ScheduleStore::create_schedule`].
#[derive(Debug, Clone)]
pub struct CreateInput {
    pub name: String,
    pub repo_root: String,
    pub workflow_name: String,
    pub params: Option<serde_json::Value>,
    pub every_secs: i64,
    pub grace_secs: Option<i64>,
    pub overlap: String,
}

/// Inputs for [`ScheduleStore::update_schedule`]: `None` = no change.
/// `params = Some(Null)` clears to absent; `grace_secs = Some(None)` clears
/// to the default grace.
#[derive(Debug, Clone, Default)]
pub struct UpdateInput {
    pub workflow_name: Option<String>,
    pub params: Option<serde_json::Value>,
    pub every_secs: Option<i64>,
    pub grace_secs: Option<Option<i64>>,
    pub overlap: Option<String>,
}

async fn blocking<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("schedule store task panicked: {e}"))?
}

fn row_to_schedule(row: &Row<'_>) -> rusqlite::Result<ScheduleRow> {
    Ok(ScheduleRow {
        id: row.get("id")?,
        name: row.get("name")?,
        repo_root: row.get("repo_root")?,
        workflow_name: row.get("workflow_name")?,
        params_raw: row.get("params")?,
        every_secs: row.get("every_secs")?,
        grace_secs: row.get("grace_secs")?,
        overlap: row.get("overlap")?,
        enabled: row.get::<_, i64>("enabled")? != 0,
        next_fire_at: row.get("next_fire_at")?,
        last_fire_at: row.get("last_fire_at")?,
        frozen_error: row.get("frozen_error")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn row_to_occurrence(row: &Row<'_>) -> rusqlite::Result<OccurrenceRow> {
    Ok(OccurrenceRow {
        id: row.get("id")?,
        schedule_id: row.get("schedule_id")?,
        slot: row.get("slot")?,
        trigger: row.get("trigger")?,
        status: row.get("status")?,
        launch_spec: row.get("launch_spec")?,
        start_deadline_at: row.get("start_deadline_at")?,
        dispatch_started_at: row.get("dispatch_started_at")?,
        run_id: row.get("run_id")?,
        error: row.get("error")?,
        created_at: row.get("created_at")?,
        finished_at: row.get("finished_at")?,
    })
}

fn select_schedule(tx: &Connection, id: &str) -> Result<Option<ScheduleRow>, String> {
    tx.query_row(
        "SELECT id,name,repo_root,workflow_name,params,every_secs,grace_secs,overlap,\
                enabled,next_fire_at,last_fire_at,frozen_error,created_at,updated_at \
         FROM schedules WHERE id = ?1",
        params![id],
        row_to_schedule,
    )
    .optional()
    .map_err(|e| format!("load schedule: {e}"))
}

/// Resolve, parse, and structurally validate the named workflow: the create/
/// update gate and the execution-time revalidation share it.
fn check_workflow(repo_root: &str, workflow_name: &str) -> Result<(), String> {
    let definition =
        crate::workflow::resolve_named_definition(std::path::Path::new(repo_root), workflow_name)?;
    let json: JsonWorkflow =
        serde_json::from_str(&definition).map_err(|e| format!("invalid workflow JSON: {e}"))?;
    // Structural compile only; the dummy hook is never invoked by it.
    let dummy = std::sync::Arc::new(|_: flare_workflow::json::StepInvocation| {
        Box::pin(async move { Err::<(String, u64, u64), String>("unused".to_string()) })
            as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
    });
    compile_workflow(&json, dummy)?;
    Ok(())
}

fn launch_snapshot(
    repo_root: &str,
    workflow_name: &str,
    params: serde_json::Value,
    schedule_name: &str,
) -> Result<String, String> {
    serde_json::to_string(&LaunchSpec {
        repo_root: repo_root.to_string(),
        workflow_name: workflow_name.to_string(),
        params,
        schedule_name: schedule_name.to_string(),
    })
    .map_err(|e| format!("serialize launch_spec: {e}"))
}

/// Blocking schedule/occurrence storage on the shared `workflows.db` file
/// (opened with the full migration list — never a second list).
#[derive(Clone)]
pub struct ScheduleStore {
    conn: Arc<Mutex<Connection>>,
}

impl ScheduleStore {
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = SqliteStore::<PipelineData>::open_shared_connection(path)
            .map_err(|e| format!("open schedule store: {e}"))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn create_schedule(
        &self,
        input: CreateInput,
        now: i64,
    ) -> Result<ScheduleRow, String> {
        validate_name(&input.name)?;
        validate_every_secs(input.every_secs)?;
        validate_grace_secs(input.grace_secs)?;
        validate_overlap(&input.overlap)?;
        let params = input.params.unwrap_or(serde_json::json!({}));
        validate_params(&params)?;
        let repo_root = canonical_repo_root(&input.repo_root)?;
        check_workflow(&repo_root, &input.workflow_name)?;
        let next_fire_at = now
            .checked_add(input.every_secs)
            .ok_or_else(|| "timestamp overflow: next_fire_at".to_string())?;
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let id = flare_workflow::WorkflowRunId::new().to_string();
            let params_raw = serde_json::to_string(&params).map_err(|e| e.to_string())?;
            let r = conn.execute(
                "INSERT INTO schedules (id,name,repo_root,workflow_name,params,every_secs,\
                        grace_secs,overlap,enabled,next_fire_at,last_fire_at,frozen_error,\
                        created_at,updated_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,1,?9,NULL,NULL,?10,?10)",
                params![
                    id,
                    input.name,
                    repo_root,
                    input.workflow_name,
                    params_raw,
                    input.every_secs,
                    input.grace_secs,
                    input.overlap,
                    next_fire_at,
                    now,
                ],
            );
            if let Err(e) = r {
                if e.to_string().contains("UNIQUE constraint") {
                    return Err(format!(
                        "schedule '{}' already exists in this repo",
                        input.name
                    ));
                }
                return Err(format!("create schedule: {e}"));
            }
            select_schedule(&conn, &id)?.ok_or_else(|| "create schedule: row missing".to_string())
        })
        .await
    }

    pub async fn get_by_id(&self, id: &str) -> Result<Option<ScheduleRow>, String> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        blocking(move || {
            let conn = conn_lock(&conn)?;
            select_schedule(&conn, &id)
        })
        .await
    }

    pub async fn get_by_name(
        &self,
        repo_root: &str,
        name: &str,
    ) -> Result<Option<ScheduleRow>, String> {
        let conn = Arc::clone(&self.conn);
        let (repo_root, name) = (repo_root.to_string(), name.to_string());
        blocking(move || {
            let conn = conn_lock(&conn)?;
            conn.query_row(
                "SELECT id,name,repo_root,workflow_name,params,every_secs,grace_secs,overlap,\
                        enabled,next_fire_at,last_fire_at,frozen_error,created_at,updated_at \
                 FROM schedules WHERE repo_root = ?1 AND name = ?2",
                params![repo_root, name],
                row_to_schedule,
            )
            .optional()
            .map_err(|e| format!("load schedule: {e}"))
        })
        .await
    }

    pub async fn list_for_repo(&self, repo_root: &str) -> Result<Vec<ScheduleRow>, String> {
        let conn = Arc::clone(&self.conn);
        let repo_root = repo_root.to_string();
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let mut stmt = conn
                .prepare(
                    "SELECT id,name,repo_root,workflow_name,params,every_secs,grace_secs,overlap,\
                            enabled,next_fire_at,last_fire_at,frozen_error,created_at,updated_at \
                     FROM schedules WHERE repo_root = ?1 ORDER BY name",
                )
                .map_err(|e| format!("prepare list: {e}"))?;
            let rows = stmt
                .query_map(params![repo_root], row_to_schedule)
                .map_err(|e| format!("list schedules: {e}"))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| format!("list schedules: {e}"))
        })
        .await
    }

    /// Every schedule, across repositories. `list --all` and MCP `all`.
    pub async fn list_all(&self) -> Result<Vec<ScheduleRow>, String> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let mut stmt = conn
                .prepare(
                    "SELECT id,name,repo_root,workflow_name,params,every_secs,grace_secs,overlap,\
                            enabled,next_fire_at,last_fire_at,frozen_error,created_at,updated_at \
                     FROM schedules ORDER BY repo_root, name",
                )
                .map_err(|e| format!("prepare list: {e}"))?;
            let rows = stmt
                .query_map([], row_to_schedule)
                .map_err(|e| format!("list schedules: {e}"))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| format!("list schedules: {e}"))
        })
        .await
    }

    pub async fn update_schedule(
        &self,
        id: &str,
        update: UpdateInput,
        now: i64,
    ) -> Result<ScheduleRow, String> {
        if update.workflow_name.as_deref().is_some_and(str::is_empty) {
            return Err("workflow name must not be empty".to_string());
        }
        if let Some(p) = &update.params
            && !p.is_null()
        {
            validate_params(p)?;
        }
        if let Some(e) = update.every_secs {
            validate_every_secs(e)?;
        }
        if let Some(g) = update.grace_secs {
            validate_grace_secs(g)?;
        }
        if let Some(o) = &update.overlap {
            validate_overlap(o)?;
        }
        let conn = Arc::clone(&self.conn);
        let (id, update) = (id.to_string(), update);
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let mut sched =
                select_schedule(&conn, &id)?.ok_or_else(|| format!("unknown schedule '{id}'"))?;
            if let Some(w) = update.workflow_name {
                check_workflow(&sched.repo_root, &w)?;
                sched.workflow_name = w;
            }
            if let Some(p) = update.params {
                sched.params_raw = if p.is_null() {
                    None
                } else {
                    Some(serde_json::to_string(&p).map_err(|e| e.to_string())?)
                };
            }
            let mut next_fire_at = sched.next_fire_at;
            if let Some(e) = update.every_secs {
                next_fire_at = now
                    .checked_add(e)
                    .ok_or_else(|| "timestamp overflow: next_fire_at".to_string())?;
                sched.every_secs = e;
            }
            if let Some(g) = update.grace_secs {
                sched.grace_secs = g;
            }
            if let Some(o) = update.overlap {
                sched.overlap = o;
            }
            // Every successful update clears a freeze; accepted occurrences
            // keep their launch snapshots.
            conn.execute(
                "UPDATE schedules SET workflow_name=?1, params=?2, every_secs=?3, grace_secs=?4,\
                        overlap=?5, next_fire_at=?6, frozen_error=NULL, updated_at=?7 WHERE id=?8",
                params![
                    sched.workflow_name,
                    sched.params_raw,
                    sched.every_secs,
                    sched.grace_secs,
                    sched.overlap,
                    next_fire_at,
                    now,
                    id,
                ],
            )
            .map_err(|e| format!("update schedule: {e}"))?;
            select_schedule(&conn, &id)?.ok_or_else(|| "update schedule: row missing".to_string())
        })
        .await
    }

    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
        now: i64,
    ) -> Result<ScheduleRow, String> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let sched =
                select_schedule(&conn, &id)?.ok_or_else(|| format!("unknown schedule '{id}'"))?;
            // Enabling resets the slot without catch-up; disabling only stops
            // new scheduled admissions.
            let next_fire_at = if enabled && !sched.enabled {
                now.checked_add(sched.every_secs)
                    .ok_or_else(|| "timestamp overflow: next_fire_at".to_string())?
            } else {
                sched.next_fire_at
            };
            conn.execute(
                "UPDATE schedules SET enabled=?1, next_fire_at=?2, updated_at=?3 WHERE id=?4",
                params![i64::from(enabled), next_fire_at, now, id],
            )
            .map_err(|e| format!("set enabled: {e}"))?;
            select_schedule(&conn, &id)?.ok_or_else(|| "set enabled: row missing".to_string())
        })
        .await
    }

    /// Delete the schedule and its occurrence history/queue via cascade.
    /// Never cancels already-claimed workflow work; receipts survive.
    pub async fn remove_schedule(&self, id: &str) -> Result<bool, String> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let n = conn
                .execute("DELETE FROM schedules WHERE id = ?1", params![id])
                .map_err(|e| format!("remove schedule: {e}"))?;
            Ok(n > 0)
        })
        .await
    }

    /// Manual fire: transactionally queue a `manual` occurrence and return
    /// its receipt. Bypasses enabled/grace/overlap, never expires, and never
    /// moves the schedule clock or creates an engine.
    pub async fn enqueue_manual(&self, schedule_id: &str, now: i64) -> Result<FireReceipt, String> {
        let conn = Arc::clone(&self.conn);
        let schedule_id = schedule_id.to_string();
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let sched = select_schedule(&conn, &schedule_id)?
                .ok_or_else(|| format!("unknown schedule '{schedule_id}'"))?;
            let params = sched
                .params_json()
                .map_err(|e| format!("schedule '{}' has corrupt params: {e}", sched.name))?;
            let spec =
                launch_snapshot(&sched.repo_root, &sched.workflow_name, params, &sched.name)?;
            let occurrence_id = flare_workflow::WorkflowRunId::new().to_string();
            conn.execute(
                "INSERT INTO schedule_runs (id,schedule_id,slot,trigger,status,launch_spec,\
                        start_deadline_at,dispatch_started_at,run_id,error,created_at,finished_at) \
                 VALUES (?1,?2,?3,'manual','queued',?4,NULL,NULL,NULL,NULL,?5,NULL)",
                params![occurrence_id, schedule_id, now, spec, now],
            )
            .map_err(|e| format!("enqueue manual: {e}"))?;
            Ok(FireReceipt {
                occurrence_id,
                status: "queued".to_string(),
            })
        })
        .await
    }

    pub async fn due_schedules(&self, now: i64) -> Result<Vec<ScheduleRow>, String> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let mut stmt = conn
                .prepare(
                    "SELECT id,name,repo_root,workflow_name,params,every_secs,grace_secs,overlap,\
                            enabled,next_fire_at,last_fire_at,frozen_error,created_at,updated_at \
                     FROM schedules WHERE enabled = 1 AND frozen_error IS NULL \
                        AND next_fire_at <= ?1 ORDER BY next_fire_at, id",
                )
                .map_err(|e| format!("prepare due: {e}"))?;
            let rows = stmt
                .query_map(params![now], row_to_schedule)
                .map_err(|e| format!("due schedules: {e}"))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| format!("due schedules: {e}"))
        })
        .await
    }

    /// One transaction per schedule (§6.2): grace/overlap admission or a
    /// terminal skip, then the slot-derived clock advance. The partial unique
    /// index is the final duplicate-claim guard.
    pub async fn claim_due_slot(&self, schedule_id: &str, now: i64) -> Result<SlotOutcome, String> {
        let conn = Arc::clone(&self.conn);
        let schedule_id = schedule_id.to_string();
        blocking(move || {
            let mut conn = conn_lock(&conn)?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("begin claim tx: {e}"))?;
            let sched = select_schedule(&tx, &schedule_id)?;
            let Some(sched) = sched else {
                tx.rollback().map_err(|e| e.to_string())?;
                return Ok(SlotOutcome::NotDue);
            };
            if !sched.enabled || sched.frozen_error.is_some() || sched.next_fire_at > now {
                tx.rollback().map_err(|e| e.to_string())?;
                return Ok(SlotOutcome::NotDue);
            }
            let slot = sched.next_fire_at;
            // Malformed params or an invalid interval freezes only this
            // schedule until an update clears it.
            let params = match sched.params_json() {
                Ok(p) => p,
                Err(e) => {
                    freeze(&tx, &schedule_id, &format!("frozen: {e}"), now)?;
                    tx.commit().map_err(|e| format!("commit freeze: {e}"))?;
                    return Ok(SlotOutcome::Frozen);
                }
            };
            // Slot-derived advance first: a failed insert or advance rolls
            // back the whole transaction, and overflow freezes instead of
            // wrapping.
            let next_fire_at = match advance_slot(slot, now, sched.every_secs) {
                Some(n) => n,
                None => {
                    freeze(&tx, &schedule_id, "frozen: slot advance overflow", now)?;
                    tx.commit().map_err(|e| format!("commit freeze: {e}"))?;
                    return Ok(SlotOutcome::Frozen);
                }
            };
            let grace = sched.effective_grace_secs();
            let outcome = if now.saturating_sub(slot) > grace {
                let id = insert_skip(&tx, &schedule_id, slot, "skipped_missed", now)?;
                SlotOutcome::SkippedMissed { occurrence_id: id }
            } else if sched.overlap == "skip" && has_open(&tx, &schedule_id)? {
                let id = insert_skip(&tx, &schedule_id, slot, "skipped_overlap", now)?;
                SlotOutcome::SkippedOverlap { occurrence_id: id }
            } else {
                let deadline = match slot.checked_add(grace) {
                    Some(d) => d,
                    None => {
                        freeze(&tx, &schedule_id, "frozen: start deadline overflow", now)?;
                        tx.commit().map_err(|e| format!("commit freeze: {e}"))?;
                        return Ok(SlotOutcome::Frozen);
                    }
                };
                let spec =
                    launch_snapshot(&sched.repo_root, &sched.workflow_name, params, &sched.name)?;
                let occurrence_id = flare_workflow::WorkflowRunId::new().to_string();
                let r = tx.execute(
                    "INSERT INTO schedule_runs (id,schedule_id,slot,trigger,status,launch_spec,\
                            start_deadline_at,dispatch_started_at,run_id,error,created_at,finished_at) \
                     VALUES (?1,?2,?3,'scheduled','queued',?4,?5,NULL,NULL,NULL,?6,NULL)",
                    params![occurrence_id, schedule_id, slot, spec, deadline, now],
                );
                match r {
                    Ok(_) => SlotOutcome::Admitted { occurrence_id },
                    Err(e) if e.to_string().contains("UNIQUE constraint") => {
                        // A competing claimant won this slot; it advances the
                        // clock below exactly once per slot.
                        SlotOutcome::NotDue
                    }
                    Err(e) => return Err(format!("admit occurrence: {e}")),
                }
            };
            tx.execute(
                "UPDATE schedules SET next_fire_at=?1, last_fire_at=?2, updated_at=?3 WHERE id=?4",
                params![next_fire_at, slot, now, schedule_id],
            )
            .map_err(|e| format!("advance clock: {e}"))?;
            tx.commit()
                .map_err(|e| format!("commit claim: {e}"))?;
            Ok(outcome)
        })
        .await
    }

    /// Claim every queued occurrence, oldest first: scheduled entries past
    /// their initial deadline become `skipped_missed` (manuals never expire),
    /// the rest flip to `dispatching` with a fresh attempt timestamp.
    /// Already-accepted queued work is never re-admitted.
    pub async fn claim_queued(&self, now: i64) -> Result<Vec<OccurrenceRow>, String> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let mut conn = conn_lock(&conn)?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("begin queue-claim tx: {e}"))?;
            let queued: Vec<OccurrenceRow> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT id,schedule_id,slot,trigger,status,launch_spec,start_deadline_at,\
                                dispatch_started_at,run_id,error,created_at,finished_at \
                         FROM schedule_runs WHERE status = 'queued' ORDER BY created_at, id",
                    )
                    .map_err(|e| format!("prepare queue: {e}"))?;
                stmt.query_map([], row_to_occurrence)
                    .map_err(|e| format!("read queue: {e}"))?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(|e| format!("read queue: {e}"))?
            };
            let mut claimed = Vec::new();
            for occ in queued {
                let expired =
                    occ.trigger == "scheduled" && occ.start_deadline_at.is_some_and(|d| now > d);
                if expired {
                    tx.execute(
                        "UPDATE schedule_runs SET status='skipped_missed', finished_at=?1 \
                         WHERE id=?2 AND status='queued'",
                        params![now, occ.id],
                    )
                    .map_err(|e| format!("expire queued: {e}"))?;
                    continue;
                }
                tx.execute(
                    "UPDATE schedule_runs SET status='dispatching', dispatch_started_at=?1 \
                     WHERE id=?2 AND status='queued'",
                    params![now, occ.id],
                )
                .map_err(|e| format!("claim queued: {e}"))?;
                let mut occ = occ;
                occ.status = "dispatching".to_string();
                occ.dispatch_started_at = Some(now);
                claimed.push(occ);
            }
            tx.commit()
                .map_err(|e| format!("commit queue-claim: {e}"))?;
            Ok(claimed)
        })
        .await
    }

    pub async fn get_occurrence(&self, id: &str) -> Result<Option<OccurrenceRow>, String> {
        let conn = Arc::clone(&self.conn);
        let id = id.to_string();
        blocking(move || {
            let conn = conn_lock(&conn)?;
            select_occurrence(&conn, &id)
        })
        .await
    }

    pub async fn occurrences_for_schedule(
        &self,
        schedule_id: &str,
        limit: i64,
    ) -> Result<Vec<OccurrenceRow>, String> {
        let conn = Arc::clone(&self.conn);
        let schedule_id = schedule_id.to_string();
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let mut stmt = conn
                .prepare(
                    "SELECT id,schedule_id,slot,trigger,status,launch_spec,start_deadline_at,\
                            dispatch_started_at,run_id,error,created_at,finished_at \
                     FROM schedule_runs WHERE schedule_id = ?1 \
                     ORDER BY created_at DESC, id DESC LIMIT ?2",
                )
                .map_err(|e| format!("prepare runs: {e}"))?;
            let rows = stmt
                .query_map(params![schedule_id, limit], row_to_occurrence)
                .map_err(|e| format!("list runs: {e}"))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| format!("list runs: {e}"))
        })
        .await
    }

    /// Every non-terminal occurrence in `status`, oldest first (reconcile scan).
    pub async fn list_open(&self, status: &str) -> Result<Vec<OccurrenceRow>, String> {
        let conn = Arc::clone(&self.conn);
        let status = status.to_string();
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let mut stmt = conn
                .prepare(
                    "SELECT id,schedule_id,slot,trigger,status,launch_spec,start_deadline_at,\
                            dispatch_started_at,run_id,error,created_at,finished_at \
                     FROM schedule_runs WHERE status = ?1 ORDER BY created_at, id",
                )
                .map_err(|e| format!("prepare open: {e}"))?;
            let rows = stmt
                .query_map(params![status], row_to_occurrence)
                .map_err(|e| format!("list open: {e}"))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| format!("list open: {e}"))
        })
        .await
    }

    /// Idempotent fire linkage: records the run on an open occurrence and
    /// moves it to `dispatched`. Never regresses a terminal row.
    pub async fn link_run(&self, occurrence_id: &str, run_id: &str) -> Result<bool, String> {
        let conn = Arc::clone(&self.conn);
        let (occurrence_id, run_id) = (occurrence_id.to_string(), run_id.to_string());
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let n = conn
                .execute(
                    "UPDATE schedule_runs SET run_id=?1, status='dispatched' \
                     WHERE id=?2 AND status IN ('dispatching','dispatched')",
                    params![run_id, occurrence_id],
                )
                .map_err(|e| format!("link run: {e}"))?;
            Ok(n > 0)
        })
        .await
    }

    /// Eager terminal write (driver completion): only a `dispatched`
    /// occurrence linked to this run moves; anything else (already terminal,
    /// unlinked, concurrently reconciled) is left for the tick fallback.
    /// Returns whether a row moved.
    pub async fn complete_linked_run(
        &self,
        run_id: &str,
        outcome: &str,
        error: Option<&str>,
        now: i64,
    ) -> Result<bool, String> {
        let conn = Arc::clone(&self.conn);
        let (run_id, outcome, error) = (
            run_id.to_string(),
            outcome.to_string(),
            error.map(str::to_string),
        );
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let n = conn
                .execute(
                    "UPDATE schedule_runs SET status=?1, error=?2, finished_at=?3 \
                     WHERE run_id=?4 AND status='dispatched'",
                    params![outcome, error, now, run_id],
                )
                .map_err(|e| format!("eager complete: {e}"))?;
            Ok(n > 0)
        })
        .await
    }

    /// Tick-fallback settle: move an open (`dispatching`/`dispatched`)
    /// occurrence to a terminal status. Never touches terminal rows.
    /// Returns whether a row moved.
    pub async fn settle_open(
        &self,
        occurrence_id: &str,
        status: &str,
        error: Option<&str>,
        now: i64,
    ) -> Result<bool, String> {
        let conn = Arc::clone(&self.conn);
        let (occurrence_id, status, error) = (
            occurrence_id.to_string(),
            status.to_string(),
            error.map(str::to_string),
        );
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let n = conn
                .execute(
                    "UPDATE schedule_runs SET status=?1, error=?2, finished_at=?3 \
                     WHERE id=?4 AND status IN ('dispatching','dispatched')",
                    params![status, error, now, occurrence_id],
                )
                .map_err(|e| format!("settle occurrence: {e}"))?;
            Ok(n > 0)
        })
        .await
    }

    /// Recovery: return a claimed-but-unlinked occurrence to the queue with a
    /// cleared attempt timestamp. Returns whether a row moved.
    pub async fn mark_queued(&self, occurrence_id: &str) -> Result<bool, String> {
        let conn = Arc::clone(&self.conn);
        let occurrence_id = occurrence_id.to_string();
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let n = conn
                .execute(
                    "UPDATE schedule_runs SET status='queued', dispatch_started_at=NULL \
                     WHERE id=?1 AND status='dispatching'",
                    params![occurrence_id],
                )
                .map_err(|e| format!("requeue occurrence: {e}"))?;
            Ok(n > 0)
        })
        .await
    }

    /// Definitive pre-start failure (or a claimed attempt lost past grace):
    /// terminal, with the reason kept on the retained row.
    pub async fn mark_dispatch_failed(
        &self,
        occurrence_id: &str,
        error: &str,
        now: i64,
    ) -> Result<bool, String> {
        let conn = Arc::clone(&self.conn);
        let (occurrence_id, error) = (occurrence_id.to_string(), error.to_string());
        blocking(move || {
            let conn = conn_lock(&conn)?;
            let n = conn
                .execute(
                    "UPDATE schedule_runs SET status='dispatch_failed', error=?1, finished_at=?2 \
                     WHERE id=?3 AND status='dispatching'",
                    params![error, now, occurrence_id],
                )
                .map_err(|e| format!("mark dispatch_failed: {e}"))?;
            Ok(n > 0)
        })
        .await
    }

    /// Retention per touched schedule, in its own transaction: newest 100
    /// non-skip terminal rows plus, independently, newest 100 skip rows.
    /// Non-terminal rows are never counted or deleted. Returns rows removed.
    pub async fn prune_schedule(&self, schedule_id: &str) -> Result<u64, String> {
        let conn = Arc::clone(&self.conn);
        let schedule_id = schedule_id.to_string();
        blocking(move || {
            let mut conn = conn_lock(&conn)?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| format!("begin prune tx: {e}"))?;
            let mut removed = 0u64;
            for statuses in [TERMINAL_NON_SKIP_STATUSES.as_slice(), SKIP_STATUSES.as_slice()] {
                let placeholders = statuses.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let sql = format!(
                    "DELETE FROM schedule_runs WHERE schedule_id = ? AND status IN ({placeholders}) \
                     AND id NOT IN (SELECT id FROM schedule_runs WHERE schedule_id = ? \
                        AND status IN ({placeholders}) ORDER BY created_at DESC, id DESC LIMIT ?)"
                );
                let mut params_vec: Vec<&dyn rusqlite::types::ToSql> = vec![&schedule_id];
                for s in statuses {
                    params_vec.push(s);
                }
                params_vec.push(&schedule_id);
                for s in statuses {
                    params_vec.push(s);
                }
                // Both caps are 100 today; the bound param keeps the shape
                // once they diverge.
                params_vec.push(&RETENTION_NON_SKIP);
                let _ = RETENTION_SKIP;
                removed += tx
                    .execute(&sql, params_vec.as_slice())
                    .map_err(|e| format!("prune: {e}"))? as u64;
            }
            tx.commit()
                .map_err(|e| format!("commit prune: {e}"))?;
            Ok(removed)
        })
        .await
    }

    #[cfg(test)]
    pub async fn test_exec(&self, sql: String) -> Result<usize, String> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn_lock(&conn)?;
            conn.execute_batch(&sql)
                .map(|_| 0)
                .map_err(|e| format!("test exec: {e}"))
        })
        .await
    }
}

fn conn_lock(
    conn: &Arc<Mutex<Connection>>,
) -> Result<std::sync::MutexGuard<'_, Connection>, String> {
    conn.lock().map_err(|e| format!("lock: {e}"))
}

fn select_occurrence(conn: &Connection, id: &str) -> Result<Option<OccurrenceRow>, String> {
    conn.query_row(
        "SELECT id,schedule_id,slot,trigger,status,launch_spec,start_deadline_at,\
                dispatch_started_at,run_id,error,created_at,finished_at \
         FROM schedule_runs WHERE id = ?1",
        params![id],
        row_to_occurrence,
    )
    .optional()
    .map_err(|e| format!("load occurrence: {e}"))
}

fn freeze(tx: &Connection, schedule_id: &str, msg: &str, now: i64) -> Result<(), String> {
    tx.execute(
        "UPDATE schedules SET frozen_error=?1, updated_at=?2 WHERE id=?3",
        params![msg, now, schedule_id],
    )
    .map_err(|e| format!("freeze schedule: {e}"))?;
    Ok(())
}

/// `next = slot + (floor((now - slot) / every) + 1) * every`, all checked.
/// Never backfills; advances from the original slot.
fn advance_slot(slot: i64, now: i64, every: i64) -> Option<i64> {
    let k = (now.saturating_sub(slot) / every).checked_add(1)?;
    slot.checked_add(k.checked_mul(every)?)
}

fn has_open(tx: &Connection, schedule_id: &str) -> Result<bool, String> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM schedule_runs WHERE schedule_id = ?1 \
         AND status IN ('queued','dispatching','dispatched'))",
        params![schedule_id],
        |row| row.get::<_, bool>(0),
    )
    .map_err(|e| format!("overlap check: {e}"))
}

fn insert_skip(
    tx: &Connection,
    schedule_id: &str,
    slot: i64,
    status: &str,
    now: i64,
) -> Result<String, String> {
    let id = flare_workflow::WorkflowRunId::new().to_string();
    tx.execute(
        "INSERT INTO schedule_runs (id,schedule_id,slot,trigger,status,launch_spec,\
                start_deadline_at,dispatch_started_at,run_id,error,created_at,finished_at) \
         VALUES (?1,?2,?3,'scheduled',?4,NULL,NULL,NULL,NULL,NULL,?5,?5)",
        params![id, schedule_id, slot, status, now],
    )
    .map_err(|e| format!("insert skip: {e}"))?;
    Ok(id)
}
