use crate::executor::{InProcessExecutor, JobFailure};
use crate::queue::Queue;
use crate::supervisor::Supervisor;
use crate::types::JobOutput;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Called from `run_in_process` when an `in_process` job's `queue.fail`
/// leaves it permanently `failed` (retries exhausted) rather than requeued
/// — the job id and its own `AgentJob` (args, in particular) are all a
/// caller needs to react, e.g. `dashboard::orphan_reconcile`'s
/// `handle_terminal_job_failure` swapping a stranded item off its
/// `dispatched` label (item #463). Not invoked for a plain subprocess job or
/// a retry-and-requeue — only a real terminal failure.
pub type TerminalFailureHook = dyn Fn(&str, &crate::types::AgentJob) + Send + Sync;

pub struct WorkerPool {
    queue: Arc<Queue>,
    handles: Vec<JoinHandle<()>>,
    running: Arc<AtomicBool>,
    executor: Option<Arc<dyn InProcessExecutor>>,
    terminal_failure_hook: Option<Arc<TerminalFailureHook>>,
}

impl WorkerPool {
    pub fn new(queue: Queue) -> Self {
        Self {
            queue: Arc::new(queue),
            handles: vec![],
            running: Arc::new(AtomicBool::new(false)),
            executor: None,
            terminal_failure_hook: None,
        }
    }

    /// Registers the executor `start`'s workers use for jobs marked
    /// `AgentJob::in_process` — see `InProcessExecutor`'s doc comment. A job
    /// marked `in_process` with no executor registered fails immediately
    /// (see `worker_loop`) rather than falling back to spawning `command` as
    /// a subprocess, so a caller that forgets to register one gets a loud,
    /// per-job failure instead of a silent behavior change.
    pub fn with_executor(mut self, executor: Arc<dyn InProcessExecutor>) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Registers a callback invoked when an `in_process` job reaches
    /// terminal `failed` state (retries exhausted) — see
    /// `TerminalFailureHook`'s doc comment.
    pub fn with_terminal_failure_hook(mut self, hook: Arc<TerminalFailureHook>) -> Self {
        self.terminal_failure_hook = Some(hook);
        self
    }

    pub fn start(&mut self, num_workers: usize) {
        self.running.store(true, Ordering::SeqCst);
        for _ in 0..num_workers {
            let queue = self.queue.clone();
            let running = self.running.clone();
            let executor = self.executor.clone();
            let terminal_failure_hook = self.terminal_failure_hook.clone();
            self.handles.push(std::thread::spawn(move || {
                worker_loop(
                    &queue,
                    &running,
                    executor.as_ref(),
                    terminal_failure_hook.as_ref(),
                );
            }));
        }
    }

    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        let handles = std::mem::take(&mut self.handles);
        // Workers may be parked in `wait_for_work` for up to its timeout —
        // wake them immediately so shutdown doesn't wait that out. A single
        // `wake_workers` call races a worker that hasn't reached the notify
        // check yet (see its doc comment): with N workers, whichever one
        // observes the flag first consumes it, leaving any other
        // not-yet-parked worker to fall through to the full fallback
        // timeout instead of noticing `running` immediately. Retrying the
        // wake until every worker has exited closes that gap — a no-op for
        // a worker mid-job (nothing is parked on the condvar to wake), and
        // bounded by the retry cap for one that's still finishing real
        // work, which the trailing `join` below waits out regardless.
        for _ in 0..40 {
            if handles.iter().all(JoinHandle::is_finished) {
                break;
            }
            self.queue.wake_workers();
            std::thread::sleep(Duration::from_millis(5));
        }
        for h in handles {
            let _ = h.join();
        }
    }
}

fn worker_loop(
    queue: &Queue,
    running: &AtomicBool,
    executor: Option<&Arc<dyn InProcessExecutor>>,
    terminal_failure_hook: Option<&Arc<TerminalFailureHook>>,
) {
    while running.load(Ordering::SeqCst) {
        match queue.dequeue() {
            Ok(Some((id, job))) if job.in_process => {
                run_in_process(queue, &id, &job, executor, terminal_failure_hook);
            }
            Ok(Some((id, job))) => {
                let mut sup = Supervisor::new(
                    id.clone(),
                    job.command.clone(),
                    job.args.clone(),
                    job.env.clone(),
                    job.cwd.clone(),
                    job.timeout_secs,
                    job.kill_after_secs,
                    queue.log_dir().to_path_buf(),
                );
                match sup.spawn() {
                    Ok((output, state)) => {
                        let success = state == crate::types::JobState::Exited;
                        if let Err(e) = queue.complete(&id, &output, success) {
                            eprintln!("agentflare-jobs: failed to complete job {id}: {e}");
                        }
                    }
                    Err(e) => {
                        if let Err(qe) = queue.fail(&id, &e.to_string(), None) {
                            eprintln!("agentflare-jobs: failed to record failure for {id}: {qe}");
                        }
                    }
                }
            }
            Ok(None) => {
                // Woken immediately by `wake_workers` (enqueue/retry/shutdown);
                // the timeout is only a safety net against a missed wakeup.
                queue.wait_for_work(Duration::from_secs(1));
            }
            Err(e) => {
                eprintln!("agentflare-jobs: dequeue error: {e}");
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

/// Runs one `in_process` job via `executor`, writing its progress to the
/// same `{id}.stdout` log-file path `Supervisor::spawn` uses for subprocess
/// jobs (see `InProcessExecutor`'s doc comment), then records the outcome
/// through `queue.complete`/`queue.fail` exactly as the subprocess path does
/// — so persisted state, retries, and the dashboard's job API/SSE all work
/// identically regardless of which way a given job actually ran.
///
/// The executor call itself runs on a fresh, short-lived thread rather than
/// this (long-lived, pooled) one, with `job.timeout_secs` as a watchdog: an
/// in-process job has no OS-level SIGKILL backstop the way a subprocess does
/// (a stuck claim/worktree/done step can't be force-killed), so without this
/// a hang there would wedge one of the pool's worker threads forever. On
/// timeout the job is marked failed and this worker moves on to the next
/// queued job; the stuck thread itself is abandoned (there is no safe way to
/// force-kill a thread in Rust) rather than actually terminated — a real,
/// deliberate trade-off against the OS-level guarantee a subprocess gets,
/// not a bug. The one genuinely open-ended part of a work item -- the agent
/// CLI itself -- is still a real subprocess under `agent_launch::run_captured`
/// with its own hard-cap/idle-timeout kill, unaffected by any of this.
fn run_in_process(
    queue: &Queue,
    id: &str,
    job: &crate::types::AgentJob,
    executor: Option<&Arc<dyn InProcessExecutor>>,
    terminal_failure_hook: Option<&Arc<TerminalFailureHook>>,
) {
    // Wraps `queue.fail` so every failure exit out of this function — not
    // just the common "executor returned `Err`" case — runs the same
    // terminal-failure notification. Missing one of these (e.g. the
    // no-executor-registered or log-file-open-failure early returns below)
    // would silently reintroduce item #463 for jobs that never even reached
    // the executor.
    let record_fail = |error: &str, retry_after_secs: Option<u64>| match queue.fail(
        id,
        error,
        retry_after_secs,
    ) {
        Ok(true) => {
            if let Some(hook) = terminal_failure_hook {
                hook(id, job);
            }
        }
        Ok(false) => {}
        Err(e) => eprintln!("agentflare-jobs: failed to record failure for {id}: {e}"),
    };

    let Some(executor) = executor else {
        record_fail(
            "job is marked in_process but no InProcessExecutor is registered on this WorkerPool",
            None,
        );
        return;
    };
    let _ = std::fs::create_dir_all(queue.log_dir());
    let stdout_path = queue.log_dir().join(format!("{id}.stdout"));
    let stderr_path = queue.log_dir().join(format!("{id}.stderr"));
    let mut log_file = match std::fs::File::create(&stdout_path) {
        Ok(f) => f,
        Err(e) => {
            record_fail(&format!("failed to open job log file: {e}"), None);
            return;
        }
    };

    let (tx, rx) = std::sync::mpsc::channel::<Result<(), JobFailure>>();
    let executor = executor.clone();
    let job_id = id.to_string();
    let args = job.args.clone();
    std::thread::spawn(move || {
        let result = executor.execute(&job_id, &args, &mut log_file);
        let _ = tx.send(result);
    });

    let outcome = rx.recv_timeout(Duration::from_secs(job.timeout_secs.max(1)));
    match outcome {
        Ok(Ok(())) => {
            let stdout_total_bytes = std::fs::metadata(&stdout_path)
                .map(|m| m.len())
                .unwrap_or(0);
            let output = JobOutput {
                exit_code: Some(0),
                timed_out: false,
                stdout_path,
                stderr_path,
                stdout_total_bytes,
                stderr_total_bytes: 0,
            };
            if let Err(e) = queue.complete(id, &output, true) {
                eprintln!("agentflare-jobs: failed to complete job {id}: {e}");
            }
        }
        Ok(Err(failure)) => {
            record_fail(&failure.message, failure.retry_after_secs);
        }
        Err(_) => {
            let msg = format!(
                "in-process job exceeded its {}s timeout and was abandoned \
                 (a coordination step may be stuck — the agent CLI subprocess \
                 itself has its own separate hard-cap/idle-timeout and is not \
                 what this timeout measures)",
                job.timeout_secs
            );
            record_fail(&msg, None);
        }
    }
}
