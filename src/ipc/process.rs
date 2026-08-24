use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub fn is_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let status = flare_process::command("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output();
        match status {
            Ok(o) => {
                let out = String::from_utf8_lossy(&o.stdout);
                out.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        let status = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status();
        matches!(status, Ok(s) if s.success())
    }
}

/// Spawns `binary` detached from the calling process. `log_path`, if given,
/// is truncated and used for both the child's stdout and stderr — `None`
/// discards them, same as before this parameter existed.
pub fn spawn_detached(
    binary: &str,
    args: &[&str],
    log_path: Option<&std::path::Path>,
) -> Result<u32, String> {
    // One shared `File` cloned for both streams: two independent
    // `File::create` handles to the same path would each get their own
    // write cursor at 0 and clobber each other instead of interleaving,
    // the same reason a shell needs `2>&1` rather than two redirects.
    let (out, err) = match log_path {
        Some(p) => {
            let f = std::fs::File::create(p)
                .map_err(|e| format!("open log file {}: {e}", p.display()))?;
            let f2 = f
                .try_clone()
                .map_err(|e| format!("open log file {}: {e}", p.display()))?;
            (std::process::Stdio::from(f), std::process::Stdio::from(f2))
        }
        None => (std::process::Stdio::null(), std::process::Stdio::null()),
    };
    #[cfg(windows)]
    {
        let mut cmd = std::process::Command::new(binary);
        cmd.args(args);
        cmd.stdout(out);
        cmd.stderr(err);
        cmd.creation_flags(
            windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP
                | windows_sys::Win32::System::Threading::DETACHED_PROCESS,
        );
        let child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
        Ok(child.id())
    }
    #[cfg(not(windows))]
    {
        let child = std::process::Command::new(binary)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(|e| format!("spawn: {e}"))?;
        Ok(child.id())
    }
}

pub fn terminate_gracefully(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        let status = flare_process::command("taskkill")
            .args(["/PID", &pid.to_string()])
            .status()
            .map_err(|e| format!("taskkill: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("taskkill for pid {pid} failed"))
        }
    }
    #[cfg(not(windows))]
    {
        let status = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .map_err(|e| format!("kill -TERM: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("kill -TERM for pid {pid} failed"))
        }
    }
}

pub fn force_kill(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        let status = flare_process::command("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status()
            .map_err(|e| format!("taskkill /F: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("taskkill /F for pid {pid} failed"))
        }
    }
    #[cfg(not(windows))]
    {
        let status = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .map_err(|e| format!("kill -KILL: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("kill -KILL for pid {pid} failed"))
        }
    }
}

#[allow(dead_code)]
pub fn find_killable_pids(binary_name: &str) -> Vec<u32> {
    let self_pid = std::process::id();
    let raw = list_pids_raw(binary_name);
    parse_other_pids(&raw, self_pid)
}

#[allow(dead_code)]
fn list_pids_raw(binary_name: &str) -> String {
    #[cfg(windows)]
    let output = flare_process::command("tasklist")
        .args([
            "/FI",
            &format!("IMAGENAME eq {binary_name}"),
            "/FO",
            "CSV",
            "/NH",
        ])
        .output();
    #[cfg(not(windows))]
    let output = flare_process::command("pgrep")
        .args(["-x", binary_name])
        .output();

    output
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[allow(dead_code)]
fn parse_other_pids(raw: &str, self_pid: u32) -> Vec<u32> {
    let mut pids = Vec::new();
    for line in raw.lines() {
        let candidate = line
            .split(&['"', ',', ' ', '\t'][..])
            .find_map(|tok| tok.trim().parse::<u32>().ok());
        let Some(pid) = candidate else {
            continue;
        };
        if pid != self_pid && !pids.contains(&pid) {
            pids.push(pid);
        }
    }
    pids
}

pub fn run_with_timeout<F, T>(f: F, timeout: Duration) -> Result<T, String>
where
    F: FnOnce() -> T,
    F: Send + 'static,
    T: Send + 'static,
{
    let handle = std::thread::spawn(f);
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if handle.is_finished() {
            return Ok(handle.join().unwrap());
        }
        if std::time::Instant::now() >= deadline {
            return Err("timed out".to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_other_pids_reads_pgrep_lines_and_drops_self() {
        assert_eq!(parse_other_pids("111\n222\n333\n", 222), vec![111, 333]);
    }

    #[test]
    fn parse_other_pids_reads_tasklist_csv() {
        let raw = "\"agentflare.exe\",\"111\",\"Console\",\"1\",\"12,345 K\"\n\
                   \"agentflare.exe\",\"222\",\"Console\",\"1\",\"12,345 K\"\n";
        assert_eq!(parse_other_pids(raw, 222), vec![111]);
    }

    #[test]
    fn parse_other_pids_dedups() {
        assert_eq!(parse_other_pids("111\n111\n", 999), vec![111]);
    }

    #[test]
    fn parse_other_pids_ignores_blank_lines() {
        assert_eq!(parse_other_pids("\n444\n", 1), vec![444]);
    }
}
