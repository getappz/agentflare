//! Item #19: jobs marked `AgentJob::in_process` run via a registered
//! `InProcessExecutor` instead of spawning `command` as a subprocess. These
//! prove the executor dispatch, log-file capture (dashboard tail parity),
//! failure-message propagation, and the stuck-job watchdog all work —
//! separate from `worker_test.rs`'s existing subprocess-path coverage, which
//! stays unchanged and passing to prove that path is untouched.

use agentflare_jobs::{
    AgentJob, InProcessExecutor, JobFailure, JobInfo, JobState, Queue, WorkerPool,
};
use std::sync::Arc;

fn test_queue() -> Queue {
    let dir = tempfile::tempdir().unwrap();
    Queue::open_memory(dir.path().join("logs")).unwrap()
}

fn wait_for_terminal(q: &Queue, id: &str, attempts: usize) -> JobInfo {
    for _ in 0..attempts {
        let i = q.get(id).unwrap();
        if matches!(
            i.state,
            JobState::Exited | JobState::Failed | JobState::Killed
        ) {
            return i;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("job {id} did not reach a terminal state in time");
}

struct EchoExecutor;
impl InProcessExecutor for EchoExecutor {
    fn execute(
        &self,
        job_id: &str,
        args: &[String],
        log: &mut dyn std::io::Write,
    ) -> Result<(), JobFailure> {
        let _ = writeln!(log, "running job {job_id} with args {args:?}");
        Ok(())
    }
}

#[test]
fn in_process_job_completes_via_the_registered_executor_and_writes_its_log() {
    let q = test_queue();
    let mut pool = WorkerPool::new(q.clone()).with_executor(Arc::new(EchoExecutor));
    pool.start(1);

    let info = q
        .enqueue(
            &AgentJob::new("label-only")
                .args(["a".to_string(), "b".to_string()])
                .in_process(),
        )
        .unwrap();
    assert!(
        info.in_process,
        "enqueue should echo the in_process flag back"
    );

    let final_info = wait_for_terminal(&q, &info.id, 200);
    pool.shutdown();

    assert_eq!(final_info.state, JobState::Exited);
    assert!(final_info.in_process);
    let output = final_info.output.expect("a completed job has output");
    assert_eq!(output.exit_code, Some(0));
    assert!(!output.timed_out);
    // The dashboard's live log tail reads exactly this path for subprocess
    // jobs (Supervisor names its files the same way) — in-process jobs must
    // land their progress output at the identical path for that to keep
    // working unchanged.
    let log = std::fs::read_to_string(&output.stdout_path).unwrap();
    assert!(log.contains(&format!("running job {} with args", info.id)));
}

struct FailingExecutor;
impl InProcessExecutor for FailingExecutor {
    fn execute(
        &self,
        _job_id: &str,
        _args: &[String],
        _log: &mut dyn std::io::Write,
    ) -> Result<(), JobFailure> {
        Err("deliberate failure".into())
    }
}

#[test]
fn in_process_job_failure_is_recorded_with_the_executors_own_message() {
    let q = test_queue();
    let mut pool = WorkerPool::new(q.clone()).with_executor(Arc::new(FailingExecutor));
    pool.start(1);

    let info = q
        .enqueue(&AgentJob::new("label-only").in_process().max_retries(0))
        .unwrap();

    let final_info = wait_for_terminal(&q, &info.id, 200);
    pool.shutdown();

    assert_eq!(final_info.state, JobState::Failed);
    assert_eq!(final_info.error.as_deref(), Some("deliberate failure"));
}

// Item #463: a job that fails cleanly (executor returns `Err`, retries
// exhausted) needs its own notification path so a caller layered on top
// (agentflare's `dashboard::orphan_reconcile::handle_terminal_job_failure`)
// can undo whatever it did when the job was first dispatched -- unlike a
// crashed-process orphan, nothing else ever observes this transition.
#[test]
fn terminal_failure_hook_fires_once_retries_are_exhausted_but_not_on_a_retry() {
    let q = test_queue();
    let seen = Arc::new(std::sync::Mutex::new(Vec::<(String, Vec<String>)>::new()));
    let seen_clone = seen.clone();
    let mut pool = WorkerPool::new(q.clone())
        .with_executor(Arc::new(FailingExecutor))
        .with_terminal_failure_hook(Arc::new(move |job_id, job| {
            seen_clone
                .lock()
                .unwrap()
                .push((job_id.to_string(), job.args.clone()));
        }));
    pool.start(1);

    let info = q
        .enqueue(
            &AgentJob::new("label-only")
                .args(["item-123".to_string()])
                .in_process()
                .max_retries(2),
        )
        .unwrap();

    let final_info = wait_for_terminal(&q, &info.id, 400);
    pool.shutdown();

    assert_eq!(final_info.state, JobState::Failed);
    assert_eq!(
        final_info.retries, 2,
        "should have retried twice before giving up"
    );

    let calls = seen.lock().unwrap();
    assert_eq!(
        calls.len(),
        1,
        "the hook must fire exactly once, on the terminal failure only -- not on either retry: {calls:?}"
    );
    assert_eq!(calls[0], (info.id.clone(), vec!["item-123".to_string()]));
}

#[test]
fn in_process_job_fails_fast_when_no_executor_is_registered() {
    let q = test_queue();
    let mut pool = WorkerPool::new(q.clone()); // no .with_executor
    pool.start(1);

    let info = q
        .enqueue(&AgentJob::new("label-only").in_process().max_retries(0))
        .unwrap();

    let final_info = wait_for_terminal(&q, &info.id, 200);
    pool.shutdown();

    assert_eq!(final_info.state, JobState::Failed);
    assert!(
        final_info
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("no InProcessExecutor"),
        "got: {:?}",
        final_info.error
    );
}

struct SlowExecutor;
impl InProcessExecutor for SlowExecutor {
    fn execute(
        &self,
        _job_id: &str,
        _args: &[String],
        _log: &mut dyn std::io::Write,
    ) -> Result<(), JobFailure> {
        std::thread::sleep(std::time::Duration::from_secs(5));
        Ok(())
    }
}

// An in-process job has no OS-level SIGKILL backstop the way a subprocess
// does, so a stuck coordination step (the trade-off item #19 explicitly
// weighs) must not wedge the worker pool forever -- `job.timeout_secs` acts
// as a watchdog that abandons the stuck executor call and lets the pool move
// on, at the cost of the stuck thread itself leaking rather than actually
// being killed (there is no safe way to force-kill a thread in Rust).
#[test]
fn in_process_job_that_hangs_is_abandoned_at_its_timeout_instead_of_wedging_the_worker() {
    let q = test_queue();
    let mut pool = WorkerPool::new(q.clone()).with_executor(Arc::new(SlowExecutor));
    pool.start(1);

    let start = std::time::Instant::now();
    let info = q
        .enqueue(
            &AgentJob::new("label-only")
                .in_process()
                .timeout(1)
                .max_retries(0),
        )
        .unwrap();

    let final_info = wait_for_terminal(&q, &info.id, 400);
    let elapsed = start.elapsed();
    pool.shutdown();

    assert_eq!(final_info.state, JobState::Failed);
    assert!(
        final_info
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("abandoned"),
        "got: {:?}",
        final_info.error
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "should fail at the 1s watchdog, not wait out the 5s executor call, took {elapsed:?}"
    );
}
