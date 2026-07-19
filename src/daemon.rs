use fs2::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::ipc::{DaemonAddr, process};

// Scaffolding for the daemon HTTP server / foreground-daemon wiring,
// landing in a follow-up PR (see task-report.md). Not reachable yet.
#[allow(dead_code)]
static IS_FOREGROUND_DAEMON: AtomicBool = AtomicBool::new(false);

pub fn daemon_pid_path() -> PathBuf {
    dirs::runtime_dir()
        .map(|d| d.join("agentflare").join("daemon.pid"))
        .unwrap_or_else(|| std::env::temp_dir().join("agentflare-daemon.pid"))
}

pub fn daemon_start_lock_path() -> PathBuf {
    dirs::runtime_dir()
        .map(|d| d.join("agentflare").join("daemon.start.lock"))
        .unwrap_or_else(|| std::env::temp_dir().join("agentflare-daemon.start.lock"))
}

#[allow(dead_code)]
pub fn is_foreground_daemon() -> bool {
    IS_FOREGROUND_DAEMON.load(Ordering::Relaxed)
}

#[allow(dead_code)]
pub fn init_foreground_daemon() -> Result<(), String> {
    IS_FOREGROUND_DAEMON.store(true, Ordering::Relaxed);
    let pid_path = daemon_pid_path();
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create pid dir {parent:?}: {e}"))?;
    }
    let pid = std::process::id();
    std::fs::write(&pid_path, pid.to_string())
        .map_err(|e| format!("write pid file {pid_path:?}: {e}"))?;
    Ok(())
}

pub fn cleanup_daemon_files() {
    let pid_path = daemon_pid_path();
    let _ = std::fs::remove_file(&pid_path);
    let lock_path = daemon_start_lock_path();
    let _ = std::fs::remove_file(&lock_path);
    let addr = DaemonAddr::default_for_pid(read_pid_from_file().unwrap_or(0));
    crate::ipc::cleanup(&addr);
}

pub struct LockGuard {
    _file: std::fs::File,
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn acquire_start_lock() -> Result<LockGuard, String> {
    let lock_path = daemon_start_lock_path();
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create lock dir {parent:?}: {e}"))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("open lock file: {e}"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if file.try_lock_exclusive().is_ok() {
            return Ok(LockGuard {
                _file: file,
                path: lock_path,
            });
        }
        if std::time::Instant::now() >= deadline {
            return Err("could not acquire daemon start lock within 5s".to_string());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

pub fn is_daemon_running() -> Option<u32> {
    let pid = read_pid_from_file()?;
    if process::is_alive(pid) {
        Some(pid)
    } else {
        cleanup_daemon_files();
        None
    }
}

fn read_pid_from_file() -> Option<u32> {
    let content = std::fs::read_to_string(daemon_pid_path()).ok()?;
    content.trim().parse::<u32>().ok()
}

pub fn start_daemon() -> Result<u32, String> {
    let _guard = acquire_start_lock()?;

    if let Some(pid) = is_daemon_running() {
        return Ok(pid);
    }

    let binary = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let _pid = process::spawn_detached(&binary.to_string_lossy(), &["--_foreground-daemon"])?;

    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(250));
        let Some(daemon_pid) = read_pid_from_file() else {
            continue;
        };
        if process::is_alive(daemon_pid) {
            return Ok(daemon_pid);
        }
    }

    Err("daemon did not start within 5s".to_string())
}

pub fn stop_daemon() -> Result<(), String> {
    let pid = match is_daemon_running() {
        Some(p) => p,
        None => return Ok(()),
    };

    let addr = DaemonAddr::default_for_pid(pid);
    let result = (|| -> Result<(), String> {
        let stream = crate::ipc::connect(&addr)?;
        let req = "POST /v1/shutdown HTTP/1.1\r\n\
                   Host: localhost\r\n\
                   Content-Length: 0\r\n\
                   Connection: close\r\n\
                   \r\n";
        stream.write_all(req.as_bytes())?;
        Ok(())
    })();

    if result.is_ok() {
        for _ in 0..8 {
            if !process::is_alive(pid) {
                cleanup_daemon_files();
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    if process::is_alive(pid) {
        let _ = process::terminate_gracefully(pid);
        std::thread::sleep(Duration::from_millis(500));
    }

    if process::is_alive(pid) {
        let _ = process::force_kill(pid);
    }

    cleanup_daemon_files();
    Ok(())
}
