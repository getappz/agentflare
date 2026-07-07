use crate::auth_db;
use crate::errors::DaemonError;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::channel;
use std::thread;
use std::time::Duration;

const RATE_LIMIT_PATTERNS: &[&str] = &[
    "429", "rate limit", "too many requests", "quota exceeded",
    "usage limit", "billing limit", "try again",
];
const MAX_RETRIES: usize = 5;

pub fn run(agent: &str, args: &[String], json: bool) {
    let mut remaining = MAX_RETRIES;
    loop {
        let status = spawn_and_capture(agent, args);
        match categorize_exit(status) {
            ExitKind::Success => return,
            ExitKind::RateLimited => {
                if remaining == 0 {
                    eprintln!("error: all retries exhausted");
                    std::process::exit(1);
                }
                remaining -= 1;
                if !json {
                    eprintln!("rate limited — rotating profile...");
                }
                let conn = auth_db::open_or_rebuild();
                if let Some((profile, _)) = auth_db::get_rotation_last(&conn, agent) {
                    auth_db::set_cooldown(&conn, agent, &profile, 30, "rate limit");
                }
                crate::auth::rotate(agent, "smart", json);
                if !json {
                    eprintln!("retrying with new profile ({remaining} retries left)...");
                }
                // ponytail: short backoff, linear increase if rate limits persist
                thread::sleep(Duration::from_secs(1 + (MAX_RETRIES - remaining) as u64));
            }
            ExitKind::Failure(code) => {
                std::process::exit(code);
            }
        }
    }
}

enum ExitKind {
    Success,
    RateLimited,
    Failure(i32),
}

fn categorize_exit(status: (i32, String)) -> ExitKind {
    let (code, stderr) = status;
    if code == 0 {
        return ExitKind::Success;
    }
    let lower = stderr.to_lowercase();
    if RATE_LIMIT_PATTERNS.iter().any(|p| lower.contains(p)) {
        return ExitKind::RateLimited;
    }
    ExitKind::Failure(code)
}

fn spawn_and_capture(agent: &str, args: &[String]) -> (i32, String) {
    let binary = find_binary(agent);
    let mut child = match Command::new(&binary)
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("error: cannot run {binary}: {e}");
            std::process::exit(1);
        }
    };

    let stderr_handle = child.stderr.take().unwrap();
    let (tx, rx) = channel();
    thread::spawn(move || {
        let reader = BufReader::new(stderr_handle);
        let mut buf = String::new();
        for line in reader.lines() {
            if let Ok(l) = line {
                eprintln!("{l}");
                buf.push_str(&l);
                buf.push('\n');
            }
        }
        tx.send(buf).ok();
    });

    let status = child.wait().expect("wait for agent");
    let stderr = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    let code = status.code().unwrap_or(1);
    (code, stderr)
}

fn find_binary(agent: &str) -> String {
    crate::agent_detect::find_binary(&[agent])
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| agent.to_string())
}

pub fn daemon_running(agent: &str) -> bool {
    let names = match agent {
        "codex" => &["codex"][..],
        "claude-code" => &["claude"][..],
        _ => return false,
    };
    if let Ok(output) = std::process::Command::new("pgrep")
        .args(names)
        .output()
    {
        return !output.stdout.is_empty();
    }
    if let Ok(output) = std::process::Command::new("tasklist")
        .arg("/FI")
        .arg(format!("IMAGENAME eq {}.exe", names[0]))
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        return stdout.contains(&format!("{}.exe", names[0]));
    }
    false
}

pub fn reload_daemon(agent: &str) -> Result<(), DaemonError> {
    if !daemon_running(agent) {
        return Ok(());
    }
    let names = match agent {
        "codex" => &["codex"][..],
        "claude-code" => &["claude"][..],
        _ => return Ok(()),
    };
    #[cfg(windows)]
    {
        for name in names {
            std::process::Command::new("taskkill")
                .args(["/IM", &format!("{name}.exe")])
                .output()
                .map_err(DaemonError::TaskKill)?;
        }
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("pkill")
            .args(names)
            .output()
            .map_err(|e| DaemonError::TaskKill(e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categorize_rate_limit_detects_429() {
        let result = categorize_exit((1, "HTTP 429 Too Many Requests".to_string()));
        assert!(matches!(result, ExitKind::RateLimited));
    }

    #[test]
    fn categorize_rate_limit_detects_quota() {
        let result = categorize_exit((1, "quota exceeded for today".to_string()));
        assert!(matches!(result, ExitKind::RateLimited));
    }

    #[test]
    fn categorize_success_on_zero() {
        let result = categorize_exit((0, String::new()));
        assert!(matches!(result, ExitKind::Success));
    }

    #[test]
    fn categorize_failure_on_unknown_error() {
        let result = categorize_exit((1, "something went wrong".to_string()));
        assert!(matches!(result, ExitKind::Failure(1)));
    }
}
