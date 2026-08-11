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
        // ...) has no business rewriting git history.
        let (sandboxed_command, sandboxed_args) =
            crate::sandbox::wrap(&self.command, &self.args, self.cwd.as_deref(), false);

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
        // guaranteed. Without this flag every dispatched job auto-allocates a
        // console window on Windows, flashing briefly even with no active
        // Claude Code session, since the daemon runs unattended in the background.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd.spawn()?;

        let stdout_pipe = child.stdout.take().expect("stdout piped");
        let stderr_pipe = child.stderr.take().expect("stderr piped");

        let stdout_path = self.stdout_path.clone();
        let stderr_path = self.stderr_path.clone();
        let stdout_handle = std::thread::spawn(move || -> std::io::Result<u64> {
            let mut file = std::fs::File::create(&stdout_path)?;
            let mut buf = [0u8; 65536];
            let mut total = 0u64;
            let mut reader = std::io::BufReader::new(stdout_pipe);
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n])?;
                total += n as u64;
            }
            Ok(total)
        });

        let stderr_handle = std::thread::spawn(move || -> std::io::Result<u64> {
            let mut file = std::fs::File::create(&stderr_path)?;
            let mut buf = [0u8; 65536];
            let mut total = 0u64;
            let mut reader = std::io::BufReader::new(stderr_pipe);
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                file.write_all(&buf[..n])?;
                total += n as u64;
            }
            Ok(total)
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

#[cfg(not(target_os = "linux"))]
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
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let pid = child.id().to_string();
        let _ = Command::new("taskkill")
            .args(["/T", "/PID", &pid])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        std::thread::sleep(kill_after);
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid])
            .creation_flags(CREATE_NO_WINDOW)
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
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid])
            .creation_flags(CREATE_NO_WINDOW)
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
    use super::*;

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
