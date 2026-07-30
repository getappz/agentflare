use agentflare_jobs::{JobState, Supervisor};
use std::fs;
use tempfile::TempDir;

fn sup(
    command: &str,
    args: &[&str],
    timeout_secs: u64,
    kill_after_secs: u64,
) -> (TempDir, Supervisor) {
    let log_tmp = tempfile::tempdir().unwrap();
    let supervisor = Supervisor::new(
        "test-job".to_string(),
        command.to_string(),
        args.iter().map(|s| s.to_string()).collect(),
        vec![],
        None,
        timeout_secs,
        kill_after_secs,
        log_tmp.path().to_path_buf(),
    );
    (log_tmp, supervisor)
}

#[test]
fn exit_success_reports_zero_code() {
    let (cmd, args) = if cfg!(windows) {
        ("cmd", vec!["/c", "exit 0"])
    } else {
        ("true", vec![])
    };
    let (_log_tmp, mut supervisor) = sup(cmd, &args, 10, 2);
    let (output, state) = supervisor.spawn().unwrap();

    assert_eq!(state, JobState::Exited);
    assert_eq!(output.exit_code, Some(0));
    assert!(!output.timed_out);
}

#[test]
fn exit_failure_reports_nonzero_code() {
    let (cmd, args) = if cfg!(windows) {
        ("cmd", vec!["/c", "exit 7"])
    } else {
        ("sh", vec!["-c", "exit 7"])
    };
    let (_log_tmp, mut supervisor) = sup(cmd, &args, 10, 2);
    let (output, state) = supervisor.spawn().unwrap();

    assert_eq!(state, JobState::Exited);
    assert_eq!(output.exit_code, Some(7));
    assert!(!output.timed_out);
}

#[test]
fn timeout_kills_long_running_process() {
    let (cmd, args) = if cfg!(windows) {
        ("cmd", vec!["/c", "timeout /t 30"])
    } else {
        ("sleep", vec!["30"])
    };
    let (_log_tmp, mut supervisor) = sup(cmd, &args, 1, 1);

    let start = std::time::Instant::now();
    let (output, state) = supervisor.spawn().unwrap();
    let elapsed = start.elapsed();

    assert_eq!(state, JobState::Killed);
    assert!(output.timed_out);
    // Bounded by timeout + kill_after, not the full 30s sleep.
    assert!(
        elapsed.as_secs() < 15,
        "expected kill well before natural exit, took {elapsed:?}"
    );
}

#[test]
fn env_vars_are_propagated_to_child() {
    let (cmd, args) = if cfg!(windows) {
        ("cmd", vec!["/c", "echo %SUPERVISOR_TEST_VAR%"])
    } else {
        ("sh", vec!["-c", "echo $SUPERVISOR_TEST_VAR"])
    };
    let log_tmp = tempfile::tempdir().unwrap();
    let mut supervisor = Supervisor::new(
        "test-job".to_string(),
        cmd.to_string(),
        args.iter().map(|s| s.to_string()).collect(),
        vec![("SUPERVISOR_TEST_VAR".to_string(), "hello-env".to_string())],
        None,
        10,
        2,
        log_tmp.path().to_path_buf(),
    );
    let (output, state) = supervisor.spawn().unwrap();

    assert_eq!(state, JobState::Exited);
    let stdout = fs::read_to_string(&output.stdout_path).unwrap();
    assert!(stdout.contains("hello-env"), "stdout was: {stdout:?}");
}

#[test]
fn cwd_is_applied_to_child() {
    let dir = tempfile::tempdir().unwrap();
    let dir_path = dir.path().to_path_buf();
    let (cmd, args) = if cfg!(windows) {
        ("cmd", vec!["/c", "cd"])
    } else {
        ("pwd", vec![])
    };
    let log_tmp = tempfile::tempdir().unwrap();
    let mut supervisor = Supervisor::new(
        "test-job".to_string(),
        cmd.to_string(),
        args.iter().map(|s| s.to_string()).collect(),
        vec![],
        Some(dir_path.clone()),
        10,
        2,
        log_tmp.path().to_path_buf(),
    );
    let (output, state) = supervisor.spawn().unwrap();

    assert_eq!(state, JobState::Exited);
    let stdout = fs::read_to_string(&output.stdout_path).unwrap();
    let canonical_dir = fs::canonicalize(&dir_path).unwrap();
    let canonical_stdout = fs::canonicalize(stdout.trim()).unwrap();
    assert_eq!(canonical_stdout, canonical_dir);
}

#[test]
fn stdout_and_stderr_are_captured_separately() {
    let (cmd, args) = if cfg!(windows) {
        ("cmd", vec!["/c", "echo out-line & echo err-line 1>&2"])
    } else {
        ("sh", vec!["-c", "echo out-line; echo err-line 1>&2"])
    };
    let (_log_tmp, mut supervisor) = sup(cmd, &args, 10, 2);
    let (output, state) = supervisor.spawn().unwrap();

    assert_eq!(state, JobState::Exited);
    let stdout = fs::read_to_string(&output.stdout_path).unwrap();
    let stderr = fs::read_to_string(&output.stderr_path).unwrap();
    assert!(stdout.contains("out-line"), "stdout was: {stdout:?}");
    assert!(!stdout.contains("err-line"), "stdout was: {stdout:?}");
    assert!(stderr.contains("err-line"), "stderr was: {stderr:?}");
    assert!(output.stdout_total_bytes > 0);
    assert!(output.stderr_total_bytes > 0);
}
