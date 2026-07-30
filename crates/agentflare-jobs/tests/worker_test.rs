use agentflare_jobs::{AgentJob, JobState, Queue, WorkerPool};

fn test_queue() -> Queue {
    let dir = tempfile::tempdir().unwrap();
    Queue::open_memory(dir.path().join("logs")).unwrap()
}

fn true_cmd() -> (&'static str, Vec<&'static str>) {
    if cfg!(windows) {
        ("cmd", vec!["/c", "exit 0"])
    } else {
        ("true", vec![])
    }
}

#[test]
fn worker_pool_picks_up_a_queued_job_and_completes_it() {
    let q = test_queue();
    let mut pool = WorkerPool::new(q.clone());
    pool.start(1);

    let (cmd, args) = true_cmd();
    let info = q.enqueue(&AgentJob::new(cmd).args(args)).unwrap();

    let mut final_info = None;
    for _ in 0..200 {
        let i = q.get(&info.id).unwrap();
        if matches!(
            i.state,
            JobState::Exited | JobState::Failed | JobState::Killed
        ) {
            final_info = Some(i);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    pool.shutdown();

    let final_info = final_info.expect("job should have finished within 2s");
    assert_eq!(final_info.state, JobState::Exited);
}

#[test]
fn worker_pool_picks_up_job_promptly_via_notify_not_just_the_fallback_poll() {
    let q = test_queue();
    let mut pool = WorkerPool::new(q.clone());
    pool.start(1);

    let (cmd, args) = true_cmd();
    let start = std::time::Instant::now();
    let info = q.enqueue(&AgentJob::new(cmd).args(args)).unwrap();

    let mut final_info = None;
    for _ in 0..100 {
        let i = q.get(&info.id).unwrap();
        if matches!(
            i.state,
            JobState::Exited | JobState::Failed | JobState::Killed
        ) {
            final_info = Some(i);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let elapsed = start.elapsed();
    pool.shutdown();

    assert!(final_info.is_some(), "job should have finished");
    // The fallback poll timeout is 1s; a working notify wakes the worker
    // essentially immediately, so a generous 300ms bound still clearly
    // distinguishes "notified" from "waited out the fallback timeout".
    assert!(
        elapsed.as_millis() < 300,
        "expected notify-driven pickup well under the 1s fallback poll, took {elapsed:?}"
    );
}

#[test]
fn shutdown_returns_promptly_even_when_workers_are_idle() {
    let q = test_queue();
    let mut pool = WorkerPool::new(q);
    pool.start(2);

    let start = std::time::Instant::now();
    pool.shutdown();
    let elapsed = start.elapsed();

    // Idle workers are parked in wait_for_work (1s timeout); shutdown must
    // wake them via wake_workers rather than waiting out that timeout.
    assert!(
        elapsed.as_millis() < 300,
        "expected shutdown to wake idle workers immediately, took {elapsed:?}"
    );
}
