//! Durable `pr-reconcile` / `pr-lifecycle` workflows (item #395 follow-up).
//! Definitions register at dashboard boot only when `[review_sweep].engine =
//! "workflow"` (or `AGENTFLARE_PR_LIFECYCLE_WORKFLOWS_REGISTER=1`); the
//! default `tick` path is unchanged.

pub(crate) const WORKFLOW_RECONCILE_ID: &str = "pr-reconcile";
pub(crate) const WORKFLOW_LIFECYCLE_ID: &str = "pr-lifecycle";

/// Per-run state for both reconcile and lifecycle workflows on the PR engine.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct PrLifecycleData {
    #[serde(default)]
    pub batches: Vec<StoredReviewBatch>,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub folder_path: String,
    #[serde(default)]
    pub item: Option<agentflare_backend::item::Item>,
    #[serde(default)]
    pub pr_number: Option<u64>,
    #[serde(default)]
    pub label_id_by_name: std::collections::HashMap<String, String>,
    /// Keys passed to `start_workflow_once` during the last fan-out (tests/debug).
    #[serde(default)]
    pub fan_out_keys: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredReviewBatch {
    pub folder_path: String,
    pub project_id: String,
    pub items: Vec<agentflare_backend::item::Item>,
    pub label_id_by_name: std::collections::HashMap<String, String>,
}

impl flare_workflow::WorkflowData for PrLifecycleData {
    fn workflow_type() -> &'static str {
        "pr-lifecycle-engine"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewSweepEngine {
    Tick,
    Workflow,
}

/// Stable `start_workflow_once` key for one open PR (spec §Identity).
pub(crate) fn lifecycle_start_key(project_id: &str, item_id: &str, pr_number: u64) -> String {
    format!("pr-lifecycle:{project_id}:{item_id}:{pr_number}")
}

pub(crate) fn review_sweep_engine() -> ReviewSweepEngine {
    if let Ok(v) = std::env::var("AGENTFLARE_REVIEW_SWEEP_ENGINE") {
        return parse_review_sweep_engine(&v);
    }
    let home = crate::paths::home();
    let Ok(layers) = flare_git_core::config_loader::locate_and_parse(home.as_path(), Some(&home))
    else {
        return ReviewSweepEngine::Tick;
    };
    [
        layers.project_local.as_ref().map(|(_, v)| v),
        layers.user_home.as_ref().map(|(_, v)| v),
    ]
    .into_iter()
    .flatten()
    .find_map(|doc| {
        doc.get("review_sweep")?
            .get("engine")?
            .as_str()
            .map(parse_review_sweep_engine)
    })
    .unwrap_or(ReviewSweepEngine::Tick)
}

fn parse_review_sweep_engine(raw: &str) -> ReviewSweepEngine {
    match raw.trim().to_ascii_lowercase().as_str() {
        "workflow" => ReviewSweepEngine::Workflow,
        _ => ReviewSweepEngine::Tick,
    }
}

fn should_register_at_boot() -> bool {
    if std::env::var("AGENTFLARE_PR_LIFECYCLE_WORKFLOWS_REGISTER")
        .as_deref()
        .is_ok_and(crate::github::bridge::config::truthy)
    {
        return true;
    }
    review_sweep_engine() == ReviewSweepEngine::Workflow
}

use flare_workflow::executor::FunctionStep;
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::{
    StepDefinition, StepResult, WorkflowContext, WorkflowDefinition, WorkflowEngine, WorkflowId,
    WorkflowResult,
};
use std::sync::Arc;

use crate::mcp_server::AgentflareMcp;

pub(crate) fn engine() -> &'static WorkflowEngine<PrLifecycleData, SqliteStore<PrLifecycleData>> {
    static ENGINE: std::sync::LazyLock<
        WorkflowEngine<PrLifecycleData, SqliteStore<PrLifecycleData>>,
    > = std::sync::LazyLock::new(|| {
        let store = SqliteStore::open_file(&crate::workflow::default_db_path())
            .expect("open workflow store for pr-lifecycle engine");
        WorkflowEngine::<PrLifecycleData, _>::with_store(store)
            .with_runtime_handle(crate::workflow::blocking_runtime_handle())
    });
    &ENGINE
}

fn stored_batch(batch: &crate::supervisor::ReviewBatch) -> StoredReviewBatch {
    StoredReviewBatch {
        folder_path: batch.folder_path.clone(),
        project_id: batch.project_id.clone(),
        items: batch.items.clone(),
        label_id_by_name: batch.label_id_by_name.clone(),
    }
}

fn empty_sweep() -> crate::supervisor::ReviewSweepResult {
    crate::supervisor::ReviewSweepResult {
        promoted: 0,
        self_repaired: 0,
        review_repaired: 0,
        skipped: 0,
        waiting: 0,
        updated: 0,
        discovered: 0,
        requeued: 0,
        stray_claimed: 0,
    }
}

/// Register both workflow definitions on the PR lifecycle engine.
pub(crate) fn register_pr_lifecycle_workflows(
    mcp: Arc<AgentflareMcp>,
    queue: agentflare_jobs::Queue,
) -> Result<(), String> {
    let (reconcile, lifecycle) = build_pr_lifecycle_definitions(mcp, queue);
    engine()
        .register_workflow(reconcile)
        .map_err(|e| e.to_string())?;
    engine()
        .register_workflow(lifecycle)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Dashboard boot hook: no-op unless workflow engine mode (or test env flag).
pub(crate) fn register_at_boot_if_enabled(
    mcp: Arc<AgentflareMcp>,
    queue: agentflare_jobs::Queue,
) -> Result<(), String> {
    if !should_register_at_boot() {
        return Ok(());
    }
    register_pr_lifecycle_workflows(mcp, queue)
}

/// Same as [`register_at_boot_if_enabled`], logs on failure (dashboard boot).
pub(crate) fn register_at_boot_if_enabled_or_log(
    mcp: Arc<AgentflareMcp>,
    queue: agentflare_jobs::Queue,
) {
    if let Err(e) = register_at_boot_if_enabled(mcp, queue) {
        crate::ui::error(&format!(
            "failed to register pr-lifecycle workflow definitions at boot: {e}"
        ));
    }
}

pub(crate) fn build_pr_lifecycle_definitions(
    mcp: Arc<AgentflareMcp>,
    queue: agentflare_jobs::Queue,
) -> (
    WorkflowDefinition<PrLifecycleData>,
    WorkflowDefinition<PrLifecycleData>,
) {
    let load_batches = build_load_batches_step(mcp.clone());
    let fan_out = build_fan_out_step();
    let reconcile = WorkflowDefinition::new(WORKFLOW_RECONCILE_ID, "pr reconcile")
        .add_step(load_batches)
        .add_step(fan_out.depends_on(&["load_batches"]));

    let dispatch = build_dispatch_status_step(mcp, queue);
    let lifecycle =
        WorkflowDefinition::new(WORKFLOW_LIFECYCLE_ID, "pr lifecycle").add_step(dispatch);

    (reconcile, lifecycle)
}

async fn load_batches(mcp: Arc<AgentflareMcp>) -> Result<Vec<StoredReviewBatch>, String> {
    let batches = tokio::task::spawn_blocking(move || {
        mcp.with_backend_db(
            |conn| -> Result<Vec<crate::supervisor::ReviewBatch>, String> {
                let dirs =
                    agentflare_backend::project_dir::list(conn).map_err(|e| e.to_string())?;
                Ok(crate::supervisor::collect_review_batches(conn, dirs))
            },
        )
    })
    .await
    .map_err(|e| format!("load_batches task: {e}"))?
    .map_err(|e| format!("load_batches db: {e}"))?
    .map_err(|e| format!("load_batches: {e}"))?;
    Ok(batches.iter().map(stored_batch).collect())
}

fn build_load_batches_step(mcp: Arc<AgentflareMcp>) -> StepDefinition<PrLifecycleData> {
    let executor = Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<PrLifecycleData>| {
            let mcp = mcp.clone();
            Box::pin(async move {
                match load_batches(mcp).await {
                    Ok(batches) => {
                        ctx.data.batches = batches;
                        Ok(StepResult::Success)
                    }
                    Err(e) => Ok(StepResult::Failed(e)),
                }
            })
        },
    ));
    StepDefinition::new("load_batches", "load_batches", executor)
}

/// Whether a schedule's `workflow_name` is a builtin Rust workflow rather
/// than a JSON file under `.agentflare/workflows`.
pub(crate) fn is_builtin_schedule_workflow(name: &str) -> bool {
    name == WORKFLOW_RECONCILE_ID
}

/// Schedule-fire definition for `pr-reconcile`. The scheduler's engine is
/// `WorkflowEngine<PipelineData>`, so this single step runs the same
/// load-batches + fan-out as the registered definition, with fan-out going to
/// the PR engine's `pr-lifecycle` runs.
pub(crate) fn scheduled_reconcile_definition(
    mcp: Arc<AgentflareMcp>,
) -> WorkflowDefinition<flare_workflow::json::PipelineData> {
    let executor = Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<flare_workflow::json::PipelineData>| {
            let mcp = mcp.clone();
            let params = ctx.params.clone();
            Box::pin(async move {
                let owner = crate::github::bridge::config::stable_instance_id();
                let batches = match load_batches(mcp).await {
                    Ok(b) => b,
                    Err(e) => return Ok(StepResult::Failed(e)),
                };
                fan_out_lifecycle_from_batches(&batches, &owner, params).await?;
                Ok(StepResult::Success)
            })
        },
    ));
    WorkflowDefinition::new(WORKFLOW_RECONCILE_ID, "pr reconcile").add_step(StepDefinition::new(
        "reconcile",
        "reconcile",
        executor,
    ))
}

/// Ensure one `pr-reconcile` schedule (`overlap=skip`, 60 s) per repo root.
/// Only meaningful when `engine = workflow`. Returns the number created.
pub(crate) async fn sync_reconcile_schedules(
    store: &crate::schedule::ScheduleStore,
    repo_roots: &[String],
) -> usize {
    let mut created = 0;
    for root in repo_roots {
        if matches!(
            store.get_by_name(root, WORKFLOW_RECONCILE_ID).await,
            Ok(Some(_))
        ) {
            continue;
        }
        let input = crate::schedule::store::CreateInput {
            name: WORKFLOW_RECONCILE_ID.into(),
            repo_root: root.clone(),
            workflow_name: WORKFLOW_RECONCILE_ID.into(),
            params: None,
            every_secs: crate::schedule::MIN_EVERY_SECS,
            grace_secs: None,
            overlap: "skip".into(),
        };
        match store
            .create_schedule(input, chrono::Utc::now().timestamp())
            .await
        {
            Ok(_) => created += 1,
            Err(e) => eprintln!("agentflare-schedule: pr-reconcile sync for {root}: {e}"),
        }
    }
    created
}

/// Drop the `pr-lifecycle` start receipts for an item whose PR merged or
/// closed, so a later PR for the same item can start a fresh run. No-op in
/// `tick` mode; soft-fails (a stale receipt only delays a restart).
pub(crate) fn clear_lifecycle_receipts(project_id: &str, item_id: &str) {
    if review_sweep_engine() != ReviewSweepEngine::Workflow {
        return;
    }
    clear_receipts_in(&crate::workflow::default_db_path(), project_id, item_id);
}

fn clear_receipts_in(db: &std::path::Path, project_id: &str, item_id: &str) {
    let pattern = format!("pr-lifecycle:{project_id}:{item_id}:%");
    let res = rusqlite::Connection::open(db).and_then(|c| {
        c.execute(
            "DELETE FROM workflow_start_receipts WHERE start_key LIKE ?1",
            [pattern],
        )
    });
    if let Err(e) = res {
        eprintln!("agentflare-supervisor: clear pr-lifecycle receipts for {item_id}: {e}");
    }
}

fn build_fan_out_step() -> StepDefinition<PrLifecycleData> {
    let executor = Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<PrLifecycleData>| {
            let discovery_owner = crate::github::bridge::config::stable_instance_id();
            Box::pin(async move {
                let keys = fan_out_lifecycle_from_batches(
                    &ctx.data.batches,
                    &discovery_owner,
                    ctx.params.clone(),
                )
                .await?;
                ctx.data.fan_out_keys = keys;
                Ok(StepResult::Success)
            })
        },
    ));
    StepDefinition::new("fan_out_lifecycle", "fan_out_lifecycle", executor)
}

fn build_dispatch_status_step(
    mcp: Arc<AgentflareMcp>,
    queue: agentflare_jobs::Queue,
) -> StepDefinition<PrLifecycleData> {
    let executor = Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<PrLifecycleData>| {
            let mcp = mcp.clone();
            let queue = queue.clone();
            Box::pin(async move {
                let Some(item) = ctx.data.item.clone() else {
                    return Ok(StepResult::Failed(
                        "pr-lifecycle: missing item snapshot in run data".into(),
                    ));
                };
                let folder_path = ctx.data.folder_path.clone();
                let project_id = ctx.data.project_id.clone();
                let labels = ctx.data.label_id_by_name.clone();
                let pr_number = ctx.data.pr_number;
                match tokio::task::spawn_blocking(move || {
                    let auth_conn = crate::auth_db::open_or_rebuild();
                    let host_policy = agentflare_resource_gate::current_policy();
                    let repo_root = std::path::PathBuf::from(&folder_path);
                    let scoped = mcp.scoped_to_project(project_id, repo_root.clone());
                    let status = match pr_number {
                        Some(number) => {
                            let resolved = (
                                crate::github::RepoId::resolve_from_remote(&repo_root),
                                crate::github::Client::new(),
                            );
                            if let (Some(repo), Ok(client)) = &resolved {
                                let batch = crate::github::graphql::batch_pr_status_chunked(
                                    client,
                                    repo,
                                    &[number],
                                );
                                let data = batch.get(&number);
                                let status = match data {
                                    Some(data) => {
                                        crate::worktree::pr_ci_status_from_batch(number, data)
                                    }
                                    None => crate::worktree::PrCiStatus::Unknown,
                                };
                                crate::worktree::pending_if_ci_running(status, client, repo)
                            } else {
                                crate::worktree::PrCiStatus::Unknown
                            }
                        }
                        None => crate::worktree::pr_ci_status(&item, &repo_root),
                    };
                    let mut sweep = empty_sweep();
                    crate::supervisor::handle_pr_status(
                        &scoped,
                        &queue,
                        &auth_conn,
                        host_policy,
                        &item,
                        status,
                        &labels,
                        &folder_path,
                        &repo_root,
                        &mut sweep,
                    );
                })
                .await
                {
                    Ok(()) => Ok(StepResult::Success),
                    Err(e) => Ok(StepResult::Failed(format!("dispatch_status task: {e}"))),
                }
            })
        },
    ));
    StepDefinition::new("dispatch_status", "dispatch_status", executor)
}

/// Fan out one lifecycle run per in-review item that carries a PR number.
pub(crate) async fn fan_out_lifecycle_from_batches(
    batches: &[StoredReviewBatch],
    discovery_owner: &str,
    params: serde_json::Value,
) -> WorkflowResult<Vec<String>> {
    let mut keys = Vec::new();
    let eng = engine();
    let lifecycle_id = WorkflowId::new(WORKFLOW_LIFECYCLE_ID);
    for batch in batches {
        for item in &batch.items {
            let Some(number) = crate::worktree::pr_number_from_metadata(item) else {
                continue;
            };
            let key = lifecycle_start_key(&batch.project_id, &item.id, number);
            let data = PrLifecycleData {
                project_id: batch.project_id.clone(),
                folder_path: batch.folder_path.clone(),
                item: Some(item.clone()),
                pr_number: Some(number),
                label_id_by_name: batch.label_id_by_name.clone(),
                ..Default::default()
            };
            let mut run_params = params.clone();
            if let Some(obj) = run_params.as_object_mut() {
                obj.insert(
                    "discovery_owner".into(),
                    serde_json::Value::from(discovery_owner),
                );
                obj.insert(
                    "project_id".into(),
                    serde_json::Value::from(batch.project_id.clone()),
                );
                obj.insert("item_id".into(), serde_json::Value::from(item.id.clone()));
                obj.insert("pr_number".into(), serde_json::json!(number));
            }
            eng.start_workflow_once_with_params(
                &key,
                lifecycle_id.clone(),
                data,
                String::new(),
                run_params,
            )
            .await?;
            keys.push(key);
        }
    }
    Ok(keys)
}

#[cfg(test)]
mod tests;
