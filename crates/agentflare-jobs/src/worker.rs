use crate::executor::{InProcessExecutor, JobFailure};
use crate::queue::Queue;
use crate::supervisor::Supervisor;
use crate::types::JobOutput;
use std::path::Path;
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
                        if let Err(qe) = queue.fail(&id, &e.to_string(), None, false) {
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
/// timeout the attempt is flagged cancelled (`cancel::job_cancelled` reads
/// true for it, so cooperative cancel checks inside the job stop it), given
/// a bounded grace period to wind down, then the job is marked failed and
/// this worker moves on to the next queued job; a thread that ignores the
/// cancellation is abandoned (there is no safe way to
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
    let record_fail = |error: &str, retry_after_secs: Option<u64>, fatal: bool| match queue.fail(
        id,
        error,
        retry_after_secs,
        fatal,
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
            false,
        );
        return;
    };
    let _ = std::fs::create_dir_all(queue.log_dir());
    let stdout_path = queue.log_dir().join(format!("{id}.stdout"));
    let stderr_path = queue.log_dir().join(format!("{id}.stderr"));
    let mut log_file = match std::fs::File::create(&stdout_path) {
        Ok(f) => f,
        Err(e) => {
            record_fail(&format!("failed to open job log file: {e}"), None, false);
            return;
        }
    };

    let (tx, rx) = std::sync::mpsc::channel::<Result<(), JobFailure>>();
    let executor = executor.clone();
    let job_id = id.to_string();
    let args = job.args.clone();
    let cancel_queue = queue.clone();
    let cancel_id = job_id.clone();
    // Registered on this thread, before the executor thread exists, so a
    // timed-out attempt's not-yet-scheduled thread can never register *after*
    // a retry's and replace it. The guard moves into the thread and is held
    // until the executor returns, so `cancel::job_cancelled(job_id)` is live
    // exactly while this job's work is.
    //
    // `abandoned` is this attempt's own watchdog flag (see the timeout arm
    // below): once set, the job reads as cancelled to everything inside this
    // attempt, so its run winds down instead of running on unobserved.
    let abandoned = Arc::new(AtomicBool::new(false));
    let abandoned_check = abandoned.clone();
    let cancel_registration = crate::cancel::register(&job_id, move || {
        abandoned_check.load(Ordering::SeqCst) || cancel_queue.is_cancelled(&cancel_id)
    });
    std::thread::spawn(move || {
        let _cancel = cancel_registration;
        let result = executor.execute(&job_id, &args, &mut log_file);
        let _ = tx.send(result);
    });

    // Records a successful attempt, whether it finished within the timeout
    // or only during the abandon grace period below. An attempt an operator
    // cancelled mid-run is finished as `killed` by `Queue::complete` itself
    // (atomically against the cancel request), not as `exited`.
    let record_success = |stdout_path: std::path::PathBuf, stderr_path: std::path::PathBuf| {
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
    };

    let outcome = rx.recv_timeout(Duration::from_secs(job.timeout_secs.max(1)));
    match outcome {
        Ok(Ok(())) => record_success(stdout_path, stderr_path),
        Ok(Err(failure)) => {
            record_output_best_effort(queue, id, &stdout_path, &stderr_path);
            record_fail(&failure.message, failure.retry_after_secs, failure.fatal);
        }
        Err(_) => {
            // Silently abandoning the thread here used to leave its whole
            // pipeline running (agent turns, claim heartbeats) while the
            // retry below started a second attempt at the same item. Flag
            // the attempt cancelled first so its cancel checks stop it, and
            // give it a bounded grace period to actually wind down before
            // the retry can be dequeued.
            abandoned.store(true, Ordering::SeqCst);
            if let Ok(Ok(())) = rx.recv_timeout(abandon_grace(job.timeout_secs)) {
                // The attempt finished its work during the grace period:
                // record it done rather than failing (and possibly
                // retrying, i.e. redoing) work that already succeeded.
                record_success(stdout_path, stderr_path);
                return;
            }
            let msg = format!(
                "in-process job exceeded its {}s timeout and was abandoned \
                 (a coordination step may be stuck — the agent CLI subprocess \
                 itself has its own separate hard-cap/idle-timeout and is not \
                 what this timeout measures)",
                job.timeout_secs
            );
            record_output_best_effort(queue, id, &stdout_path, &stderr_path);
            record_fail(&msg, None, false);
        }
    }
}

/// How long a timed-out attempt gets to observe its cancellation and stop
/// before its job is recorded as failed (and possibly retried) anyway.
/// Capped by the job's own timeout so a short-timeout job's watchdog still
/// fires promptly.
fn abandon_grace(timeout_secs: u64) -> Duration {
    const MAX_ABANDON_GRACE: Duration = Duration::from_secs(30);
    Duration::from_secs(timeout_secs.max(1)).min(MAX_ABANDON_GRACE)
}

/// The log file at `stdout_path` was already written by the executor
/// before it failed or timed out (see `run_in_process`'s call sites) — this
/// persists that path/size the same way the success branch does, so a
/// failed job's log stays reachable via `stdout_log_path` instead of
/// existing on disk with no DB row pointing at it. Best-effort: a failure
/// here just means the job's own failure/timeout error still gets recorded
/// via `record_fail` right after, unaffected by this.
fn record_output_best_effort(queue: &Queue, id: &str, stdout_path: &Path, stderr_path: &Path) {
    let stdout_total_bytes = std::fs::metadata(stdout_path).map(|m| m.len()).unwrap_or(0);
    if let Err(e) = queue.record_output(id, stdout_path, stderr_path, stdout_total_bytes, 0) {
        eprintln!("agentflare-jobs: failed to record output for {id}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentJob, JobState};

    /// Runs past its own watchdog unless it cooperatively polls the cancel
    /// registry -- the shape of the work-item pipeline's wait loop. Reports
    /// whether it saw the cancellation.
    struct CooperativeSlowExecutor {
        saw_cancel: std::sync::mpsc::SyncSender<bool>,
    }

    impl InProcessExecutor for CooperativeSlowExecutor {
        fn execute(
            &self,
            job_id: &str,
            _args: &[String],
            _log: &mut dyn std::io::Write,
        ) -> Result<(), JobFailure> {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if crate::cancel::job_cancelled(job_id) {
                    let _ = self.saw_cancel.send(true);
                    return Err(crate::cancel::CANCELLED_MESSAGE.into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = self.saw_cancel.send(false);
            Ok(())
        }
    }

    // The watchdog must not just walk away from a timed-out attempt: the
    // attempt keeps running its pipeline (agent turns, claim heartbeats) next
    // to the retry. Flagging it cancelled lets its own cancel checks stop it.
    #[test]
    fn timed_out_in_process_job_is_flagged_cancelled_so_the_attempt_stops() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open_memory(dir.path().join("logs")).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let mut pool = WorkerPool::new(q.clone())
            .with_executor(Arc::new(CooperativeSlowExecutor { saw_cancel: tx }));
        pool.start(1);

        let info = q
            .enqueue(
                &AgentJob::new("label-only")
                    .in_process()
                    .timeout(1)
                    .max_retries(0),
            )
            .unwrap();

        let saw_cancel = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the executor must stop well before its own 10s deadline");
        assert!(saw_cancel, "the timed-out attempt must read as cancelled");
        let mut final_info = q.get(&info.id).unwrap();
        for _ in 0..400 {
            if final_info.state == JobState::Failed {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            final_info = q.get(&info.id).unwrap();
        }
        pool.shutdown();
        assert_eq!(final_info.state, JobState::Failed);
        assert!(
            final_info
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("abandoned"),
            "the watchdog's own reason is what gets recorded, got: {:?}",
            final_info.error
        );
    }

    /// Ignores cancellation and finishes successfully a little after its
    /// job's 1s watchdog fires -- inside the abandon grace period.
    struct FinishesDuringGraceExecutor {
        runs: Arc<std::sync::atomic::AtomicU32>,
    }

    impl InProcessExecutor for FinishesDuringGraceExecutor {
        fn execute(
            &self,
            _job_id: &str,
            _args: &[String],
            _log: &mut dyn std::io::Write,
        ) -> Result<(), JobFailure> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(1300));
            Ok(())
        }
    }

    // An attempt that completes its work during the grace period succeeded:
    // recording it as a timeout failure would retry (redo) finished work.
    #[test]
    fn attempt_finishing_during_the_abandon_grace_is_recorded_as_done() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open_memory(dir.path().join("logs")).unwrap();
        let runs = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut pool = WorkerPool::new(q.clone())
            .with_executor(Arc::new(FinishesDuringGraceExecutor { runs: runs.clone() }));
        pool.start(1);

        let info = q
            .enqueue(
                &AgentJob::new("label-only")
                    .in_process()
                    .timeout(1)
                    .max_retries(3),
            )
            .unwrap();
        let mut final_info = q.get(&info.id).unwrap();
        for _ in 0..500 {
            if final_info.state.is_terminal() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            final_info = q.get(&info.id).unwrap();
        }
        // Give a (wrong) retry the chance to be dequeued before asserting.
        std::thread::sleep(Duration::from_millis(200));
        pool.shutdown();
        assert_eq!(final_info.state, JobState::Exited, "{:?}", final_info.error);
        assert_eq!(q.get(&info.id).unwrap().retries, 0, "never retried");
        assert_eq!(runs.load(Ordering::SeqCst), 1, "work done exactly once");
    }

    /// Signals once it's running, then waits for the test's go-ahead and
    /// returns `Ok` without ever polling the cancel registry.
    struct IgnoresCancelExecutor {
        started: std::sync::mpsc::SyncSender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl InProcessExecutor for IgnoresCancelExecutor {
        fn execute(
            &self,
            _job_id: &str,
            _args: &[String],
            _log: &mut dyn std::io::Write,
        ) -> Result<(), JobFailure> {
            let _ = self.started.send(());
            let _ = self
                .release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
            Ok(())
        }
    }

    // An operator cancel of a running job is deliberate: an executor that
    // finishes its work anyway must not turn it into `exited`.
    #[test]
    fn cancelled_running_job_whose_executor_returns_ok_finishes_killed() {
        let dir = tempfile::tempdir().unwrap();
        let q = Queue::open_memory(dir.path().join("logs")).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut pool = WorkerPool::new(q.clone()).with_executor(Arc::new(IgnoresCancelExecutor {
            started: started_tx,
            release: std::sync::Mutex::new(release_rx),
        }));
        pool.start(1);

        let info = q
            .enqueue(
                &AgentJob::new("label-only")
                    .in_process()
                    .timeout(30)
                    .max_retries(0),
            )
            .unwrap();
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("executor must start");
        assert!(q.request_cancel(&info.id).unwrap(), "running job flagged");
        release_tx.send(()).unwrap();

        let mut final_info = q.get(&info.id).unwrap();
        for _ in 0..500 {
            if final_info.state.is_terminal() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
            final_info = q.get(&info.id).unwrap();
        }
        pool.shutdown();
        assert_eq!(final_info.state, JobState::Killed);
        assert_eq!(
            final_info.error.as_deref(),
            Some(crate::cancel::CANCELLED_MESSAGE)
        );
    }
}
