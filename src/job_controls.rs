//! Operator controls over dispatched work: cancel a job, cancel/pause/resume
//! an item's run, and the plumbing the CLI (`agentflare job ...`,
//! `agentflare item ...`), the MCP `item` tool, the dashboard and the chat
//! commands share, so every surface does exactly the same thing.
//!
//! Pause keeps the worktree and the persisted workflow run (paused at a step
//! boundary, its in-flight agent turn killed through the job cancel flag)
//! but releases the item claim: no process runs while an item is paused, so
//! nothing could heartbeat a held claim, and a claim left to expire would
//! just be stolen by the TTL anyway. The `paused` label is what holds the
//! item instead -- discovery never dispatches it and the claim-liveness
//! sweep never re-queues it -- and resume swaps it back for
//! `ready-for-work`, whose dispatch adopts the paused run (see
//! `work_item_pipeline::run_or_resume_with_sender`) and continues it from
//! its persisted step.

use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::{WorkflowEngine, WorkflowRunId, WorkflowStatus};

type Engine = WorkflowEngine<
    crate::work_item_pipeline::WorkItemData,
    SqliteStore<crate::work_item_pipeline::WorkItemData>,
>;

/// Runs `f` against the process-wide work-item engine on its runtime, from a
/// fresh thread: callers include async contexts (the MCP server, dashboard
/// handlers) where blocking on another runtime from the current thread
/// would panic.
fn with_engine<T, Fut>(f: impl FnOnce(&'static Engine) -> Fut + Send + 'static) -> T
where
    T: Send + 'static,
    Fut: std::future::Future<Output = T>,
{
    std::thread::spawn(move || {
        crate::workflow::blocking_runtime().block_on(f(crate::work_item_pipeline::engine()))
    })
    .join()
    .expect("workflow engine control thread panicked")
}

/// The item's persisted work-item run id (`metadata.workflow_run_id`).
pub(crate) fn run_id_of(metadata: &str) -> Option<WorkflowRunId> {
    serde_json::from_str::<serde_json::Value>(metadata)
        .ok()?
        .get("workflow_run_id")?
        .as_str()?
        .parse()
        .ok()
}

/// Current status of `run_id`, `None` when the run no longer exists.
pub(crate) fn run_status(run_id: WorkflowRunId) -> Option<WorkflowStatus> {
    with_engine(move |eng| async move { eng.get_status(run_id).await.ok().map(|s| s.status) })
}

/// Pauses the run (see `WorkflowEngine::pause_workflow`). Returns whether it
/// was running and is now paused.
pub(crate) fn pause_run(run_id: WorkflowRunId) -> Result<bool, String> {
    with_engine(move |eng| async move {
        match eng.pause_workflow(run_id).await {
            Ok(paused) => Ok(paused),
            Err(flare_workflow::WorkflowError::NotFound(_)) => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    })
}

/// Cancels the run if it hasn't finished. Returns whether it was cancelled.
pub(crate) fn cancel_run(run_id: WorkflowRunId) -> Result<bool, String> {
    with_engine(move |eng| async move {
        let status = match eng.get_status(run_id).await {
            Ok(state) => state.status,
            Err(flare_workflow::WorkflowError::NotFound(_)) => return Ok(false),
            Err(e) => return Err(e.to_string()),
        };
        if matches!(
            status,
            WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
        ) {
            return Ok(false);
        }
        eng.cancel_workflow(run_id)
            .await
            .map(|()| true)
            .map_err(|e| e.to_string())
    })
}

/// A paused run whose driver hasn't reached its step boundary yet (still
/// driven here, or still leased by another live process). Resuming it now
/// would race that driver, so resume asks the caller to retry instead.
pub(crate) fn run_still_stopping(run_id: WorkflowRunId) -> bool {
    with_engine(move |eng| async move {
        eng.get_status(run_id).await.is_ok_and(|state| {
            state.status == WorkflowStatus::Paused
                && (eng.is_driving(run_id) || eng.is_leased_elsewhere(&state))
        })
    })
}

/// The job queue in `agentflare.db` the daemon's workers drain.
pub(crate) fn open_queue() -> Result<agentflare_jobs::Queue, String> {
    agentflare_jobs::Queue::open(
        &crate::db::agentflare_db_path(),
        crate::state::state_dir().join("job-logs"),
    )
    .map_err(|e| e.to_string())
}

/// The item a work-item job was dispatched for (`args[0]`), if it is one.
pub(crate) fn job_item(queue: &agentflare_jobs::Queue, job_id: &str) -> Option<String> {
    let job = queue.get(job_id).ok()?;
    if !job.in_process {
        return None;
    }
    job.args.first().cloned()
}

/// An `AgentflareMcp` scoped to the project `item_ref` belongs to, so a
/// control issued from anywhere (the daemon's dashboard, a CLI outside the
/// repo) acts on the right project. A `#<seq>` ref can't name a project, so
/// it stays scoped to the caller's own project.
pub(crate) fn mcp_for_item(item_ref: &str) -> crate::mcp_server::AgentflareMcp {
    let mcp = crate::mcp_server::AgentflareMcp::default();
    let folder = mcp
        .with_backend_db(|conn| {
            let item = agentflare_backend::item::get(conn, item_ref).ok()?;
            agentflare_backend::project_dir::list(conn)
                .ok()?
                .into_iter()
                .find(|d| d.project_id == item.project_id)
                .map(|d| d.folder_path)
        })
        .ok()
        .flatten();
    match folder {
        Some(folder) => crate::mcp_server::AgentflareMcp::for_project_dir(folder.into()),
        None => mcp,
    }
}

/// Adds or removes the project label `name` on an item, creating the label
/// in the item's project first when adding one that doesn't exist yet.
pub(crate) fn set_item_label(
    conn: &rusqlite::Connection,
    item: &agentflare_backend::item::Item,
    name: &str,
    present: bool,
) -> agentflare_backend::error::Result<()> {
    let labels = agentflare_backend::label::list_by_project(conn, &item.project_id)?;
    let existing = labels.iter().find(|l| l.name == name).map(|l| l.id.clone());
    match (existing, present) {
        (Some(id), true) => agentflare_backend::item::add_label(conn, &item.id, &id),
        (Some(id), false) => agentflare_backend::item::remove_label(conn, &item.id, &id),
        (None, false) => Ok(()),
        (None, true) => {
            let project = agentflare_backend::project::get(conn, &item.project_id)?;
            let label = agentflare_backend::label::create(
                conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(project.id),
                    workspace_id: project.workspace_id,
                    name: name.to_string(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            )?;
            agentflare_backend::item::add_label(conn, &item.id, &label.id)
        }
    }
}

/// `agentflare job cancel <job-id>` / `POST /api/jobs/:id/cancel`: stops one
/// job for good. A queued job never starts; a running in-process job has its
/// agent killed (the executor polls the cancel flag) and is finished as
/// `killed`, never retried. A running subprocess job can't observe the flag
/// and is refused (`agentflare_jobs::Error::NotCancellable`) rather than
/// reported cancelled while it runs on. For a work-item job the item is parked on
/// `needs-manual-dispatch` so neither discovery nor the claim-liveness sweep
/// re-queues it behind the operator's back; `redispatch` re-arms it.
pub(crate) fn cancel_job(job_id: &str) -> Result<serde_json::Value, String> {
    let queue = open_queue()?;
    let cancelled = queue.request_cancel(job_id).map_err(|e| e.to_string())?;
    let mut resp = serde_json::json!({ "job_id": job_id, "cancelled": cancelled });
    if !cancelled {
        resp["note"] = "job had already finished".into();
        return Ok(resp);
    }
    if let Some(item_id) = job_item(&queue, job_id) {
        resp["item_id"] = item_id.clone().into();
        let mcp = mcp_for_item(&item_id);
        let author = crate::claims::owner_id();
        let _ = mcp.with_backend_db(|conn| -> agentflare_backend::error::Result<()> {
            let item = agentflare_backend::item::get(conn, &item_id)?;
            for name in [
                crate::supervisor::READY_LABEL,
                crate::supervisor::DISPATCHED_LABEL,
            ] {
                set_item_label(conn, &item, name, false)?;
            }
            set_item_label(conn, &item, crate::supervisor::NEEDS_MANUAL_LABEL, true)?;
            agentflare_backend::comment::create(
                conn,
                &item_id,
                &author,
                &format!(
                    "## agentflare — job cancelled\n\njob {job_id} was cancelled by an operator; \
                     the item is parked on `needs-manual-dispatch` (redispatch it to try again)."
                ),
            )?;
            Ok(())
        });
    }
    Ok(resp)
}

/// Resolves a `job-id|item` argument to an item ref: a known work-item job
/// id maps to its item, anything else is taken as an item ref as-is.
pub(crate) fn item_ref_for(job_or_item: &str) -> String {
    open_queue()
        .ok()
        .and_then(|queue| job_item(&queue, job_or_item))
        .unwrap_or_else(|| job_or_item.to_string())
}

/// Runs one `item` tool action for `item_ref` in the item's own project and
/// returns its JSON reply -- the shared entry point for the CLI, dashboard
/// and chat controls.
pub(crate) fn item_action(
    item_ref: &str,
    action: &str,
    reason: Option<String>,
    agent: Option<String>,
) -> Result<String, String> {
    let mcp = mcp_for_item(item_ref);
    mcp.item_control(crate::mcp_server::types::ItemRequest {
        action: action.into(),
        id: Some(item_ref.to_string()),
        reason,
        assignee_agent: agent,
        ..Default::default()
    })
    .map_err(|e| e.message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_of_reads_the_persisted_run_id() {
        let run_id = WorkflowRunId::new();
        let metadata = serde_json::json!({ "workflow_run_id": run_id.to_string() }).to_string();
        assert_eq!(run_id_of(&metadata), Some(run_id));
        assert_eq!(run_id_of("{}"), None);
        assert_eq!(run_id_of("not json"), None);
    }
}
