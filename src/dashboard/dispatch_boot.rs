//! Work-dispatch boot for the dashboard daemon: starting the worker pool and
//! the supervisor ticks (`start_work_dispatch`) once the work-item pipeline
//! has recovered, retrying a failed boot-time recovery
//! (`spawn_pipeline_recovery_retry`), and sizing the worker pool
//! (`work_max_concurrency`). Split out of `server.rs` to keep it under the
//! LOC gate (`scripts/loc-gate.sh`); re-exported by the parent as
//! `use dispatch_boot::*`, so `server.rs`'s own tests are unchanged.

use super::*;

/// How often a failed boot-time `engine().recover()` is retried.
pub(super) const PIPELINE_RECOVERY_RETRY_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(60);

/// Starts everything that claims or dispatches work -- the worker pool and
/// the supervisor's discovery/review/approval ticks. Only ever called once
/// the work-item pipeline's in-flight runs have been recovered, so a fresh
/// dispatch can never race a resumed run for the same item.
pub(super) fn start_work_dispatch(queue: &agentflare_jobs::Queue) {
    let mut worker_pool = agentflare_jobs::WorkerPool::new(queue.clone())
        .with_executor(std::sync::Arc::new(crate::cli::work::WorkItemExecutor))
        .with_terminal_failure_hook(std::sync::Arc::new(|_job_id, job| {
            super::super::orphan_reconcile::handle_terminal_job_failure(job);
        }));
    worker_pool.start(work_max_concurrency());
    spawn_supervisor_discovery(
        queue.clone(),
        std::sync::Arc::new(crate::mcp_server::AgentflareMcp::default()),
        SUPERVISOR_DISCOVERY_INTERVAL,
    );
    spawn_supervisor_review_sweep(
        queue.clone(),
        std::sync::Arc::new(crate::mcp_server::AgentflareMcp::default()),
        SUPERVISOR_REVIEW_SWEEP_INTERVAL,
    );
    spawn_supervisor_telegram_approvals(
        std::sync::Arc::new(crate::mcp_server::AgentflareMcp::default()),
        SUPERVISOR_TELEGRAM_POLL_INTERVAL,
    );
}

/// Daemon-owned workflow-schedule ticker (item #366 Phase 1): boots the
/// `Scheduler` on its own `WorkflowEngine<PipelineData>` + store pair over
/// `default_db_path()` and ticks every 60 s. Deliberately independent of
/// `start_work_dispatch`'s smoke-test / register / recover gate — a
/// work-item recovery failure leaves schedules firing. Never calls
/// `recover`/`resume_run`/`try_resume` on the scheduler engine: occurrence
/// linkage goes through start receipts, and resuming here would break
/// at-most-once fire semantics.
pub(super) fn spawn_schedule_ticker() -> tokio::task::JoinHandle<()> {
    spawn_schedule_ticker_every(
        crate::workflow::default_db_path(),
        crate::workflow::agent_send_hook(),
        crate::schedule::TICK_INTERVAL_SECS,
    )
}

/// Test seam: same ticker over an isolated database with an injectable send
/// hook and tick interval. Production always passes the 60 s constant.
pub(crate) fn spawn_schedule_ticker_every(
    db_path: std::path::PathBuf,
    send: flare_workflow::json::SendMessage,
    interval_secs: u64,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let store = match crate::schedule::ScheduleStore::open(&db_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("agentflare-schedule: ticker off (store open failed): {e}");
                return;
            }
        };
        let wf_store = match flare_workflow::sqlite_store::SqliteStore::open_file(&db_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("agentflare-schedule: ticker off (workflow store open failed): {e}");
                return;
            }
        };
        let engine = flare_workflow::engine::WorkflowEngine::with_store(wf_store)
            .with_runtime_handle(crate::workflow::blocking_runtime_handle());
        let scheduler = crate::schedule::Scheduler::new(store, engine, send);
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(1)));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            match scheduler.tick_once(chrono::Utc::now()).await {
                Ok(summary) => {
                    if summary.admitted > 0
                        || summary.skipped_missed > 0
                        || summary.skipped_overlap > 0
                        || summary.reconciled > 0
                        || summary.fired > 0
                        || !summary.errors.is_empty()
                    {
                        eprintln!(
                            "agentflare-schedule: tick admitted={} missed={} overlap={} \
                             reconciled={} fired={} pruned={} errors={:?}",
                            summary.admitted,
                            summary.skipped_missed,
                            summary.skipped_overlap,
                            summary.reconciled,
                            summary.fired,
                            summary.pruned,
                            summary.errors,
                        );
                    }
                }
                Err(e) => {
                    eprintln!("agentflare-schedule: tick failed: {e}");
                }
            }
        }
    })
}

/// Retries `engine().recover()` every `PIPELINE_RECOVERY_RETRY_INTERVAL`
/// after it failed at boot, logging each failure, and starts work dispatch
/// the first time it succeeds.
pub(super) fn spawn_pipeline_recovery_retry(queue: agentflare_jobs::Queue) {
    tokio::spawn(async move {
        let mut attempt: u32 = 1;
        loop {
            tokio::time::sleep(PIPELINE_RECOVERY_RETRY_INTERVAL).await;
            attempt += 1;
            match crate::work_item_pipeline::engine()
                .recover_with(crate::work_item_pipeline::should_recover_at_boot)
                .await
            {
                Ok(_) => {
                    eprintln!(
                        "agentflare: work-item pipeline recovery succeeded on attempt {attempt} \
                         -- starting work dispatch"
                    );
                    start_work_dispatch(&queue);
                    return;
                }
                Err(e) => crate::ui::error(&format!(
                    "work-item pipeline recovery attempt {attempt} failed: {e} -- retrying in {}s",
                    PIPELINE_RECOVERY_RETRY_INTERVAL.as_secs()
                )),
            }
        }
    });
}

/// Parses `AGENTFLARE_WORK_MAX_CONCURRENCY` into an explicit override, or
/// `None` when absent, zero, or unparseable — in which case the caller
/// falls back to `concurrency::resolve_pool_size`'s CPU+memory-aware
/// default rather than silently starting a `WorkerPool` with no workers
/// (which would wedge the queue forever with no error). Split into a pure
/// parse step so the override logic is testable without mutating
/// process-global env state — env vars are shared across the whole test
/// binary, unlike this narrow seam.
pub(super) fn parse_work_max_concurrency(raw: Option<&str>) -> Option<usize> {
    raw.and_then(|s| s.parse::<usize>().ok()).filter(|n| *n > 0)
}

/// Defaults to a CPU+memory-aware pool size (ported from codegraph's
/// `ResolverPool.resolvePoolSize`, see `agentflare_resource_gate::pool_size`)
/// instead of a flat hardcoded value, so a resource-starved box and a beefy
/// dev box no longer run the same fixed concurrency.
pub(super) fn work_max_concurrency() -> usize {
    parse_work_max_concurrency(
        std::env::var("AGENTFLARE_WORK_MAX_CONCURRENCY")
            .ok()
            .as_deref(),
    )
    .unwrap_or_else(|| {
        let available_parallelism = std::thread::available_parallelism().map_or(1, |n| n.get());
        agentflare_resource_gate::pool_size::resolve_pool_size(
            available_parallelism,
            agentflare_resource_gate::pool_size::memory_budget_bytes(),
        )
    })
}
