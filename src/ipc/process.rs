use std::time::Duration;

pub fn is_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let status = std::process::Command::new("tasklist")
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

pub fn spawn_detached(binary: &str, args: &[&str]) -> Result<u32, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new(binary);
        cmd.args(args);
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
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn: {e}"))?;
        Ok(child.id())
    }
}

pub fn terminate_gracefully(pid: u32) -> Result<(), String> {
    #[cfg(windows)]
    {
        let status = std::process::Command::new("taskkill")
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
        let status = std::process::Command::new("taskkill")
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
    let output = std::process::Command::new("tasklist")
        .args([
            "/FI",
            &format!("IMAGENAME eq {binary_name}"),
            "/FO",
            "CSV",
            "/NH",
        ])
        .output();
    #[cfg(not(windows))]
    let output = std::process::Command::new("pgrep")
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

#[allow(dead_code)]
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
