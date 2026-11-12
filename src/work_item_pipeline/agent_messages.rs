// Pipeline fallback custody, included into work_item_pipeline.rs.
const AGENT_MESSAGE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const AGENT_MESSAGE_GRACE_SECS: i64 = 20;

#[derive(Default)]
struct MessageOutbox {
    initialized: bool,
    accepted: std::collections::BTreeSet<String>,
    entries: std::collections::BTreeMap<i64, String>,
    owner: String,
    target_branch: String,
}

impl MessageOutbox {
    fn apply(&self, data: &mut WorkItemData) {
        data.owner = self.owner.clone();
        data.target_branch = self.target_branch.clone();
        data.accepted_message_receipts
            .extend(self.accepted.iter().cloned());
        // A stale context may still carry a correction the current turn consumed.
        for (id, text) in &data.pending_message_receipts {
            if !self.entries.contains_key(id) {
                data.pending_corrections.retain(|pending| pending != text);
            }
        }
        data.pending_message_receipts.clone_from(&self.entries);
        for text in self.entries.values() {
            if !data.pending_corrections.contains(text) {
                data.pending_corrections.push(text.clone());
            }
        }
    }
}

struct RunMessageOutbox {
    last_poll: std::sync::Mutex<std::time::Instant>,
    operation: tokio::sync::Mutex<()>,
    data: std::sync::Mutex<MessageOutbox>,
}
type Outboxes =
    std::collections::HashMap<flare_workflow::WorkflowRunId, std::sync::Arc<RunMessageOutbox>>;
fn message_outboxes() -> &'static std::sync::Mutex<Outboxes> {
    static OUTBOXES: std::sync::OnceLock<std::sync::Mutex<Outboxes>> = std::sync::OnceLock::new();
    OUTBOXES.get_or_init(Default::default)
}
fn message_outbox(run_id: flare_workflow::WorkflowRunId) -> std::sync::Arc<RunMessageOutbox> {
    message_outboxes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(run_id)
        .or_insert_with(|| {
            std::sync::Arc::new(RunMessageOutbox {
                last_poll: std::sync::Mutex::new(
                    std::time::Instant::now() - AGENT_MESSAGE_POLL_INTERVAL,
                ),
                operation: tokio::sync::Mutex::new(()),
                data: Default::default(),
            })
        })
        .clone()
}
fn consume_agent_messages(run_id: flare_workflow::WorkflowRunId, texts: &[String]) {
    message_outbox(run_id)
        .data
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entries
        .retain(|_, text| !texts.contains(text));
}
fn receipt_connection(path: &std::path::Path) -> Result<Option<rusqlite::Connection>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let conn = rusqlite::Connection::open(path).map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| e.to_string())?;
    crate::messages::migrate(&conn).map_err(|e| e.to_string())?;
    Ok(Some(conn))
}

async fn prepare_message_delivery(
    eng: &WorkflowEngine<WorkItemData, SqliteStore<WorkItemData>>,
    run_id: flare_workflow::WorkflowRunId,
    owner: &str,
    target_branch: Option<&str>,
    message_path: &std::path::Path,
    take_new: bool,
) -> Result<(), String> {
    let outbox = message_outbox(run_id);
    let _operation = outbox.operation.lock().await;
    let state = eng.get_status(run_id).await.map_err(|e| e.to_string())?;
    let terminal = matches!(
        state.status,
        WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
    );
    if terminal && (take_new || target_branch.is_none()) {
        return Err(format!("message custody: run {run_id} is terminal"));
    }
    if eng.held_by_live_executor(&state) {
        return Err(format!("message custody: run {run_id} is leased elsewhere"));
    }
    if target_branch.is_none() && state.context.data.owner != owner {
        return Err(format!("message custody: run {run_id} changed owner"));
    }
    let path = message_path.to_path_buf();
    let key = owner.to_string();
    let receipts = tokio::task::spawn_blocking(move || {
        // Terminal repair adoption may update metadata, never replay messages.
        if terminal {
            return Ok(Vec::new());
        }
        let Some(conn) = receipt_connection(&path)? else {
            return Ok(Vec::new());
        };
        let run = run_id.to_string();
        if take_new {
            let now = crate::claims::now();
            crate::messages::receipts::reserve_for_run(
                &conn,
                &run,
                &key,
                now - AGENT_MESSAGE_GRACE_SECS,
                now,
            )
        } else {
            crate::messages::receipts::unresolved_for_run(&conn, &run)
        }
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;
    if take_new && receipts.is_empty() {
        return Ok(());
    }
    #[cfg(test)]
    message_receipt_tests::handoff_barrier("before_custody", run_id);
    {
        let mut pending = outbox
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !pending.initialized {
            pending
                .entries
                .clone_from(&state.context.data.pending_message_receipts);
            pending.initialized = true;
        }
        pending
            .accepted
            .extend(state.context.data.accepted_message_receipts.iter().cloned());
        pending.owner = owner.to_string();
        pending.target_branch = target_branch
            .unwrap_or(&state.context.data.target_branch)
            .to_string();
        for r in &receipts {
            if pending.accepted.insert(r.receipt_id.clone()) {
                pending.entries.insert(
                    r.message.id,
                    crate::messages::format_delivery(std::slice::from_ref(&r.message)),
                );
            }
        }
    }
    let patch = outbox.clone();
    eng.patch_run_data(run_id, move |data| {
        patch
            .data
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply(data);
    })
    .await
    .map_err(|e| e.to_string())?;
    if receipts.is_empty() {
        return Ok(());
    }
    // A successful patch is custody, never proof that an agent processed the text.
    let checkpoint = eng.get_status(run_id).await.map_err(|e| e.to_string())?;
    if matches!(
        checkpoint.status,
        WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
    ) {
        return Err(format!(
            "message custody: run {run_id} ended during handoff"
        ));
    }
    if receipts.iter().any(|r| {
        !checkpoint
            .context
            .data
            .accepted_message_receipts
            .contains(&r.receipt_id)
    }) {
        return Err(format!(
            "message custody: run {run_id} did not retain its receipt checkpoint"
        ));
    }
    #[cfg(test)]
    message_receipt_tests::handoff_barrier("after_custody", run_id);
    let path = message_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let conn = receipt_connection(&path)?
            .ok_or_else(|| "message custody database disappeared".to_string())?;
        for r in receipts {
            crate::messages::receipts::confirm(
                &conn,
                &r.receipt_id,
                &run_id.to_string(),
                &r.message.to_key,
                crate::claims::now(),
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn poll_agent_messages(
    eng: &WorkflowEngine<WorkItemData, SqliteStore<WorkItemData>>,
    run_id: flare_workflow::WorkflowRunId,
    owner: &str,
) {
    let outbox = message_outbox(run_id);
    {
        let mut last = outbox
            .last_poll
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.elapsed() < AGENT_MESSAGE_POLL_INTERVAL {
            return;
        }
        *last = std::time::Instant::now();
    }
    if let Err(error) = prepare_message_delivery(
        eng,
        run_id,
        owner,
        None,
        &crate::db::agentflare_db_path(),
        true,
    )
    .await
    {
        eprintln!("work_item_pipeline: run {run_id}: {error}; custody remains unresolved");
    }
}

async fn recover_with_message_custody(
    eng: &WorkflowEngine<WorkItemData, SqliteStore<WorkItemData>>,
    message_path: &std::path::Path,
    filter: impl Fn(&flare_workflow::WorkflowState<WorkItemData>) -> bool,
) -> Result<Vec<flare_workflow::WorkflowRunId>, String> {
    let active = eng
        .state_store()
        .list_active()
        .await
        .map_err(|e| e.to_string())?;
    let mut resumed = Vec::new();
    for state in active {
        if !filter(&state) || eng.is_driving(state.run_id) {
            continue;
        }
        if eng.held_by_live_executor(&state) {
            continue;
        }
        match resume_with_message_custody(
            eng,
            state.run_id,
            &state.context.data.owner,
            None,
            message_path,
        )
        .await
        {
            Ok(true) => resumed.push(state.run_id),
            Ok(false) => {}
            Err(error) => eprintln!(
                "work_item_pipeline: could not recover run {}: {error}; leaving it undispatched",
                state.run_id
            ),
        }
    }
    Ok(resumed)
}

async fn resume_with_message_custody(
    eng: &WorkflowEngine<WorkItemData, SqliteStore<WorkItemData>>,
    run_id: flare_workflow::WorkflowRunId,
    owner: &str,
    target_branch: Option<&str>,
    message_path: &std::path::Path,
) -> Result<bool, String> {
    eng.resume_run_with(run_id, || async {
        prepare_message_delivery(eng, run_id, owner, target_branch, message_path, false)
            .await
            .map_err(WorkflowError::Store)
    })
    .await
    .map_err(|e| e.to_string())
}
pub(crate) async fn recover_at_boot() -> Result<Vec<flare_workflow::WorkflowRunId>, String> {
    recover_with_message_custody(
        engine(),
        &crate::db::agentflare_db_path(),
        should_recover_at_boot,
    )
    .await
}

#[cfg(test)]
mod agent_message_outbox_tests {
    use super::*;
    #[test]
    fn workflow_json_exposes_durable_receipt_evidence() {
        let json = serde_json::to_value(WorkItemData::default()).unwrap();
        assert!(
            json.get("accepted_message_receipts").is_some(),
            "receiver custody must survive workflow serialization"
        );
    }
    #[test]
    fn outbox_survives_a_stale_write_back_and_lets_go_once_consumed() {
        let mut ob = MessageOutbox::default();
        ob.entries.insert(1, "msg".into());
        ob.accepted.insert("receipt".into());
        let mut stored = WorkItemData::default();
        ob.apply(&mut stored);
        assert_eq!(stored.pending_corrections, vec!["msg"]);
        stored.pending_corrections.clear();
        ob.apply(&mut stored);
        assert_eq!(stored.pending_corrections, vec!["msg"]);
        ob.entries.clear();
        ob.apply(&mut stored);
        assert!(stored.pending_corrections.is_empty());
        assert!(stored.pending_message_receipts.is_empty());
        assert!(stored.accepted_message_receipts.contains("receipt"));
    }
}
