use crate::types::{JobOutput, JobState};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct Supervisor {
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    pub timeout: Duration,
    pub kill_after: Duration,
    pub stdout_path: PathBuf,
    pub stderr_path: PathBuf,
    pub log_dir: PathBuf,
}

impl Supervisor {
    /// `id` names the log files (`{id}.stdout`/`{id}.stderr`) — callers
    /// running this under `agentflare-jobs::Queue` must pass the job's own
    /// queue id, not a fresh one, so a running job's log path is derivable
    /// from its id alone (`queue.log_dir().join(format!("{id}.stdout"))`)
    /// without waiting for the job to finish and report it back.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        cwd: Option<PathBuf>,
        timeout_secs: u64,
        kill_after_secs: u64,
        log_dir: PathBuf,
    ) -> Self {
        let stdout_path = log_dir.join(format!("{id}.stdout"));
        let stderr_path = log_dir.join(format!("{id}.stderr"));
        Self {
            command,
            args,
            env,
            cwd,
            timeout: Duration::from_secs(timeout_secs),
            kill_after: Duration::from_secs(kill_after_secs),
            stdout_path,
            stderr_path,
            log_dir,
        }
    }

    pub fn spawn(&mut self) -> std::io::Result<(JobOutput, JobState)> {
        let _ = std::fs::create_dir_all(&self.log_dir);

        // Native Linux and WSL2 run the job inside a bwrap sandbox; Windows
        // and macOS have no equivalent here yet, so they get the command
        // back unchanged (see `crate::sandbox`). `git_writable = false`: an
        // arbitrary job command dispatched through here (build/test/lint,
        // ...) has no business rewriting git history. `diagnostic_out:
        // None` -- the bounded diagnostic-log capture (item #139) only
        // applies to a headless coding-agent CLI dispatch, not an arbitrary
        // build/test/lint job.
        let (sandboxed_command, sandboxed_args) =
            crate::sandbox::wrap(&self.command, &self.args, self.cwd.as_deref(), false, None);

        let mut cmd = Command::new(&sandboxed_command);
        cmd.args(&sandboxed_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null());

        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        if let Some(ref cwd) = self.cwd {
            cmd.current_dir(cwd);
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        // This supervisor is driven by the daemon's own background discovery
        // tick (`spawn_supervisor_discovery`, every `SUPERVISOR_DISCOVERY_INTERVAL`)
        // as well as interactive job requests — no console-attached parent is
        // guaranteed. Without the no-window flag every dispatched job
        // auto-allocates a console window on Windows, flashing briefly even
        // with no active Claude Code session, since the daemon runs
        // unattended in the background.
        flare_process::no_window(&mut cmd);

        let mut child = cmd.spawn()?;

        let stdout_pipe = child.stdout.take().expect("stdout piped");
        let stderr_pipe = child.stderr.take().expect("stderr piped");

        let stdout_path = self.stdout_path.clone();
        let stderr_path = self.stderr_path.clone();
        let stdout_handle = std::thread::spawn(move || -> std::io::Result<u64> {
            let file = std::fs::File::create(&stdout_path)?;
            let reader = std::io::BufReader::new(stdout_pipe);
            Ok(pump_capped(reader, file).0)
        });

        let stderr_handle = std::thread::spawn(move || -> std::io::Result<u64> {
            let file = std::fs::File::create(&stderr_path)?;
            let reader = std::io::BufReader::new(stderr_pipe);
            Ok(pump_capped(reader, file).0)
        });

        let start = Instant::now();
        let state = loop {
            match child.try_wait()? {
                Some(status) => {
                    let exit_code = status.code();
                    break (JobState::Exited, exit_code, false);
                }
                None => {
                    if start.elapsed() >= self.timeout {
                        kill_graceful(&mut child, self.kill_after);
                        let exit_code = child.wait().ok().and_then(|s| s.code());
                        break (JobState::Killed, exit_code, true);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };

        let (final_state, exit_code, did_timeout) = state;
        let stdout_bytes = stdout_handle.join().ok().and_then(|r| r.ok()).unwrap_or(0);
        let stderr_bytes = stderr_handle.join().ok().and_then(|r| r.ok()).unwrap_or(0);

        let output = JobOutput {
            exit_code,
            timed_out: did_timeout,
            stdout_path: self.stdout_path.clone(),
            stderr_path: self.stderr_path.clone(),
            stdout_total_bytes: stdout_bytes,
            stderr_total_bytes: stderr_bytes,
        };

        Ok((output, final_state))
    }
}

/// Per-stream cap on bytes written to a job's log file (64 MiB): a runaway
/// job spewing output must not fill the disk. Past the cap the pump keeps
/// *draining* the pipe (so the child never blocks on a full pipe) but stops
/// writing, appending one truncation marker instead -- the same bounded-queue
/// discipline as OpenShell's exec output (`sandbox-limits.md`).
pub const MAX_LOG_STREAM_BYTES: u64 = 64 * 1024 * 1024;

/// Pumps `reader` into `file` up to [`MAX_LOG_STREAM_BYTES`], continuing to
/// drain past the cap so the child never blocks. Returns
/// `(bytes_written, truncated)`.
fn pump_capped<R: Read, W: Write>(reader: R, file: W) -> (u64, bool) {
    pump_capped_with(reader, file, MAX_LOG_STREAM_BYTES)
}

fn pump_capped_with<R: Read, W: Write>(mut reader: R, mut file: W, cap: u64) -> (u64, bool) {
    let mut buf = [0u8; 65536];
    let mut total = 0u64;
    let mut truncated = false;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if !truncated {
            // A single read can overshoot the cap: persist the head up to
            // the cap so the retained prefix is always the true prefix.
            let room = cap.saturating_sub(total) as usize;
            let take = room.min(n);
            if take > 0 && file.write_all(&buf[..take]).is_err() {
                break;
            }
            total += take as u64;
            if take < n {
                truncated = true;
                let _ = writeln!(file, "\n[job log truncated at {cap} bytes]");
                total = cap;
            }
        }
        // Past the cap: keep draining so the child never blocks on a full
        // pipe; nothing more is stored.
    }
    (total, truncated)
}

/// PIDs of every live descendant of `pid` (children, grandchildren, ...),
/// walked via `/proc/<pid>/task/*/children` -- Linux-only (no `/proc` on
/// macOS/Windows). Needed because a descendant that called
/// `process_group(0)` on itself (see `agent_launch::run_captured`, which
/// does exactly this so *it* can kill a runaway agent CLI) has left the
/// process group `-{pid}` targets below; signaling it by group alone won't
/// reach it, so `kill_graceful` also signals every PID this returns
/// directly.
#[cfg(target_os = "linux")]
fn descendant_pids(pid: u32) -> Vec<u32> {
    fn children_of(pid: u32) -> Vec<u32> {
        let task_dir = format!("/proc/{pid}/task");
        let Ok(entries) = std::fs::read_dir(&task_dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let children_path = entry.path().join("children");
            let Ok(contents) = std::fs::read_to_string(&children_path) else {
                continue;
            };
            out.extend(
                contents
                    .split_whitespace()
                    .filter_map(|s| s.parse::<u32>().ok()),
            );
        }
        out
    }

    let mut all = Vec::new();
    let mut frontier = children_of(pid);
    while let Some(next_pid) = frontier.pop() {
        all.push(next_pid);
        frontier.extend(children_of(next_pid));
    }
    all
}

// Only reachable from `kill_graceful`'s `#[cfg(unix)]` block below, so this
// stub is for macOS/BSD specifically (unix-but-not-Linux) — plain
// `not(target_os = "linux")` also matches Windows, where the stub compiled
// but its only caller didn't, leaving it dead code and failing `-D
// dead-code` on that platform.
#[cfg(all(unix, not(target_os = "linux")))]
fn descendant_pids(_pid: u32) -> Vec<u32> {
    Vec::new()
}

fn kill_graceful(child: &mut std::process::Child, kill_after: Duration) {
    #[cfg(unix)]
    {
        let pid = child.id();
        let signal = |sig: &str, pid: u32| {
            let _ = Command::new("kill")
                .arg("-s")
                .arg(sig)
                .arg("--")
                .arg(format!("-{pid}"))
                .status();
            for descendant in descendant_pids(pid) {
                let _ = Command::new("kill")
                    .arg("-s")
                    .arg(sig)
                    .arg("--")
                    .arg(descendant.to_string())
                    .status();
            }
        };
        signal("TERM", pid);
        std::thread::sleep(kill_after);
        signal("KILL", pid);
    }
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = flare_process::command("taskkill")
            .args(["/T", "/PID", &pid])
            .status();
        std::thread::sleep(kill_after);
        let _ = flare_process::command("taskkill")
            .args(["/T", "/F", "/PID", &pid])
            .status();
        // `taskkill /T` builds its kill list from a single point-in-time
        // process-tree snapshot. A grandchild spawned in the narrow window
        // between that snapshot and termination (e.g. `child` hadn't yet
        // exec'd its own subprocess) can survive the call entirely
        // undetected (item #78) and keep running for its full natural
        // lifetime with no supervisor left to bound it. Windows keeps a
        // dead process's original parent-PID association around for
        // lookups until the PID is reused, so a second forceful pass a
        // moment later still finds and kills any such straggler; it's a
        // harmless no-op once the tree is already gone.
        std::thread::sleep(Duration::from_millis(250));
        let _ = flare_process::command("taskkill")
            .args(["/T", "/F", "/PID", &pid])
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child.kill();
    }
    // Both external kill commands above swallow their errors (missing
    // binary, permission denied, etc.), so the process may still be alive.
    // Fall back to a direct OS-level kill so the caller's child.wait() can
    // never block forever on a process we failed to actually terminate.
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
    }
}

#[cfg(test)]
mod tests {
    // Only `#[cfg(target_os = "linux")]` tests live in this module today, so
    // this glob import is unused (and fails `-D unused-imports`) on every
    // other platform.
    use super::pump_capped;
    use super::pump_capped_with;
    #[cfg(target_os = "linux")]
    use super::*;

    #[test]
    fn pump_capped_passes_small_output_through() {
        let input = b"hello job output\n";
        let mut out = Vec::new();
        let (total, truncated) = pump_capped(&input[..], &mut out);
        assert!(!truncated);
        assert_eq!(total, input.len() as u64);
        assert_eq!(out, input);
    }

    #[test]
    fn pump_capped_truncates_marker_and_drains_rest() {
        let input = vec![b'x'; 300];
        let mut out = Vec::new();
        let (total, truncated) = pump_capped_with(&input[..], &mut out, 100);
        assert!(truncated);
        assert_eq!(total, 100);
        // First 100 bytes verbatim, then exactly one marker; the remaining
        // 200 input bytes were consumed (drained) without being stored.
        assert!(out.starts_with(&[b'x'; 100]));
        let marker = b"[job log truncated at 100 bytes]";
        assert_eq!(
            out.windows(marker.len()).filter(|w| *w == marker).count(),
            1
        );
        assert!(out.len() < input.len());
    }

    // `setsid` moves the grandchild into a brand-new session/process group of
    // its own -- exactly what `agent_launch::run_captured` does to its own
    // child so *it* can kill a runaway agent CLI independently. That leaves
    // the grandchild outside the group `kill -TERM -- -<pid>` targets, so a
    // fix that only sends the group signal would leave it running past the
    // timeout. Only `/proc` (Linux) backs `descendant_pids`, so this is
    // scoped to Linux CI rather than xfailing on macOS/Windows runners.
    #[cfg(target_os = "linux")]
    #[test]
    fn spawn_times_out_and_kills_a_descendant_that_escaped_into_its_own_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("still-alive");
        let mut supervisor = Supervisor::new(
            "test".to_string(),
            "sh".to_string(),
            vec![
                "-c".to_string(),
                format!("setsid sh -c 'sleep 5; touch {}' & wait", marker.display()),
            ],
            vec![],
            None,
            0, // times out immediately — the point is what happens on timeout
            0,
            dir.path().to_path_buf(),
        );

        let (output, _state) = supervisor.spawn().unwrap();
        assert!(output.timed_out, "should report timeout");
        // The 5s `sleep` inside the setsid'd grandchild would still have it
        // alive if `kill_graceful` only signaled the direct child's process
        // group; give it a moment past `spawn()` returning, then confirm it
        // never reached its `touch`.
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            !marker.exists(),
            "a descendant that escaped into its own process group must still be \
             killed on timeout, not just orphaned"
        );
    }
}
