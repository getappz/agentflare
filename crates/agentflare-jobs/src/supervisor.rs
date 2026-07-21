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
    pub fn new(
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
        cwd: Option<PathBuf>,
        timeout_secs: u64,
        kill_after_secs: u64,
        log_dir: PathBuf,
    ) -> Self {
        let id = db_kit::ids::new_id();
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

        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args)
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

fn kill_graceful(child: &mut std::process::Child, kill_after: Duration) {
    #[cfg(unix)]
    {
        let pid = child.id();
        let _ = Command::new("kill")
            .arg("-s")
            .arg("TERM")
            .arg("--")
            .arg(format!("-{pid}"))
            .status();
        std::thread::sleep(kill_after);
        let _ = Command::new("kill")
            .arg("-s")
            .arg("KILL")
            .arg("--")
            .arg(format!("-{pid}"))
            .status();
    }
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("taskkill").args(["/T", "/PID", &pid]).status();
        std::thread::sleep(kill_after);
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid])
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child.kill();
    }
}
