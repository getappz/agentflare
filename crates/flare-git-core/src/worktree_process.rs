//! Deadline-bounded subprocess execution for `worktree`: runs a child in
//! its own process group and kills the whole tree on timeout. Child module
//! of `worktree` (split out for size).

use std::io::Read;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Kills `child` and its whole process tree — not just the direct child —
/// so a grandchild (e.g. a `git` credential helper) can't outlive a timeout.
pub(super) fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // `kill -KILL -<pid>` packs the signal and the (negative, i.e.
        // process-group-targeting) pid into two separate `-`-prefixed argv
        // entries. Some `kill` implementations misparse the second as
        // another option rather than as the target once a signal option has
        // already been consumed. `-s SIGNAME` plus a `--` end-of-options
        // marker before the pid is the portable, unambiguous idiom.
        let _ = Command::new("kill")
            .arg("-s")
            .arg("KILL")
            .arg("--")
            .arg(format!("-{}", child.id()))
            .status();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .status();
        // `taskkill /T` builds its kill list from a single point-in-time
        // process-tree snapshot. A grandchild spawned in the narrow window
        // between that snapshot and termination (e.g. this child hadn't yet
        // exec'd its own subprocess) can survive the call entirely
        // undetected (item #78). Windows keeps a dead process's original
        // parent-PID association around for lookups until the PID is
        // reused, so a second pass a moment later still finds and kills any
        // such straggler; it's a harmless no-op once the tree is already
        // gone.
        std::thread::sleep(Duration::from_millis(250));
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child.kill();
    }
}

/// Runs `program` with a deadline, returning its output. Puts the child in
/// its own process group (Unix) and kills that whole group — not just the
/// direct child — if it outlives `timeout_secs`, via `kill_tree`; a plain
/// `child.kill()` would leave a grandchild (e.g. a `git` credential helper)
/// running and the process genuinely un-reaped, not just "late". Stdout/
/// stderr are drained on separate threads so a child that fills an OS pipe
/// buffer can't deadlock the wait loop.
pub(crate) fn run_output_timeout(
    program: impl AsRef<std::ffi::OsStr>,
    args: &[&str],
    cwd: &Path,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let program = program.as_ref().to_owned();
    let mut cmd = flare_process::command(&program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    crate::shell::apply_filtered_path(&mut cmd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: spawn failed: {e}", program.to_string_lossy()))?;
    let mut stdout_pipe = child.stdout.take().expect("stdout piped above");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped above");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    kill_tree(&mut child);
                    let _ = child.wait();
                    return Err(format!(
                        "{}: timed out after {timeout_secs}s",
                        program.to_string_lossy()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("{}: {e}", program.to_string_lossy())),
        }
    };
    Ok(std::process::Output {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}

/// Fixed pause between the two attempts in [`fetch_with_retry`] — long enough
/// to ride out a one-off DNS/TCP blip, short enough not to meaningfully delay
/// a claim on top of `fetch_timeout_secs`.
pub(super) const FETCH_RETRY_DELAY: Duration = Duration::from_secs(2);

/// [`run_output_timeout`], retried once on failure (a non-zero exit counts as
/// a failure worth retrying, same as a spawn/timeout error).
///
/// The fetches this wraps run once per worktree creation, on the critical
/// path of claiming a work item, against a remote this process doesn't
/// control the reachability of — a single dropped DNS query or transient TCP
/// reset (observed on real workstations, not hypothetical) would otherwise
/// silently seed a new branch off a stale local ref, exactly the staleness
/// this fetch exists to prevent. One retry is deliberately not a full
/// exponential-backoff loop: `fetch_timeout_secs` already bounds each
/// attempt, so retrying more still can't hang a claim indefinitely, but it
/// isn't worth adding unbounded delay chasing a remote that is genuinely
/// unreachable — the caller's existing fallback-to-local-ref handles that
/// case correctly, it just needs to be logged instead of silent.
pub(super) fn fetch_with_retry(
    program: impl AsRef<std::ffi::OsStr>,
    args: &[&str],
    cwd: &Path,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let program = program.as_ref().to_owned();
    let first = run_output_timeout(&program, args, cwd, timeout_secs);
    if matches!(&first, Ok(out) if out.status.success()) {
        return first;
    }
    std::thread::sleep(FETCH_RETRY_DELAY);
    run_output_timeout(&program, args, cwd, timeout_secs)
}
