use agentflare_jobs::{JobState, Supervisor};
use std::fs;
use tempfile::TempDir;

/// item #78: on Windows, `kill_graceful` only confirms its *direct* child is
/// reaped (`child.wait()`) — it never verifies a grandchild process is
/// actually gone, trusting `taskkill /T`'s point-in-time tree snapshot
/// blindly. If that snapshot misses a grandchild (e.g. spawned in the brief
/// window between the snapshot and termination), it keeps running
/// unsupervised. On a resource-constrained CI runner that's exactly the kind
/// of background load that can starve whatever test nextest schedules next
/// (see item #78's investigation). Poll for `image_name` processes whose
/// command line contains `cmdline_needle` so a regression here fails loudly
/// and locally instead of surfacing as an unrelated test's mystery timeout.
#[cfg(windows)]
fn assert_no_surviving_process(image_name: &str, cmdline_needle: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let ps_query = format!(
            "(Get-CimInstance Win32_Process -Filter \"Name='{image_name}'\" | \
             Where-Object {{ $_.CommandLine -like '*{cmdline_needle}*' }} | \
             Select-Object -ExpandProperty ProcessId) -join ','"
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps_query])
            .output();
        let survivors = out
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if survivors.is_empty() {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "kill_graceful left {image_name} process(es) matching \
                 '{cmdline_needle}' running after timeout (pid(s): {survivors}) \
                 — the process tree was not fully terminated"
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

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
        // `timeout /t` refuses to run with redirected stdin ("INPUT
        // REDIRECTION IS NOT SUPPORTED") and exits instantly instead of
        // sleeping — `ping` against loopback is the standard
        // redirection-safe stand-in for a long delay on Windows.
        ("cmd", vec!["/c", "ping -n 31 127.0.0.1 >nul"])
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
    // See `assert_no_surviving_process`'s doc comment (item #78).
    #[cfg(windows)]
    assert_no_surviving_process("ping.exe", "-n 31");
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
