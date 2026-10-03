//! Snapshot of live processes' working directories, for "is anything still
//! running in this directory" checks before a destructive cleanup.

use std::path::PathBuf;

pub struct LiveProc {
    pub pid: u32,
    pub name: String,
    pub cwd: PathBuf,
}

/// Every process whose cwd is readable, minus this process and its
/// ancestors (the shell that launched us is not "using" the directory).
#[cfg(unix)]
#[must_use]
pub fn live_procs() -> Vec<LiveProc> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        // Processes only: a thread ("task") has its own id and name, which
        // would slip past the ancestor exclusion and name a thread in the
        // "in use by ..." message.
        ProcessRefreshKind::nothing()
            .with_cwd(UpdateKind::Always)
            .without_tasks(),
    );
    let mut skip = std::collections::HashSet::new();
    let mut pid = Some(Pid::from_u32(std::process::id()));
    while let Some(p) = pid {
        if !skip.insert(p) {
            break;
        }
        pid = sys.process(p).and_then(sysinfo::Process::parent);
    }
    sys.processes()
        .iter()
        .filter(|(pid, _)| !skip.contains(*pid))
        .filter_map(|(pid, proc_)| {
            let cwd = proc_.cwd()?;
            Some(LiveProc {
                pid: pid.as_u32(),
                name: proc_.name().to_string_lossy().into_owned(),
                cwd: cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()),
            })
        })
        .collect()
}

/// No cwd access elsewhere (`sysinfo` is a unix-only dependency of this
/// crate; see Cargo.toml). On Windows an open handle already makes the
/// rename-aside step fail with the directory intact.
#[cfg(not(unix))]
#[must_use]
pub fn live_procs() -> Vec<LiveProc> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::live_procs;

    #[cfg(unix)]
    #[test]
    fn sees_a_child_inside_a_dir_but_not_ourselves() {
        let dir = tempfile::TempDir::new().unwrap();
        let canon = dir.path().canonicalize().unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(dir.path())
            .spawn()
            .unwrap();
        let procs = live_procs();
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            procs.iter().any(|p| p.pid == child.id() && p.cwd == canon),
            "child with cwd in the temp dir must be listed"
        );
        assert!(
            procs.iter().all(|p| p.pid != std::process::id()),
            "the current process must be excluded"
        );
    }

    /// A thread has its own id and name (`tokio-rt-worker`, `libuv-worker`),
    /// which is useless in a "this directory is in use by ..." message and
    /// slips past the ancestor exclusion.
    #[cfg(target_os = "linux")]
    #[test]
    fn lists_processes_not_their_threads() {
        let tgid = |pid: u32| -> Option<u32> {
            let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
            let line = status.lines().find(|l| l.starts_with("Tgid:"))?;
            line.split_whitespace().nth(1)?.parse().ok()
        };
        // Hold a named thread open so this test binary itself has one.
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::Builder::new()
            .name("cwd-test-worker".into())
            .spawn(move || rx.recv().ok())
            .unwrap();
        let procs = live_procs();
        drop(tx);
        worker.join().unwrap();
        let threads: Vec<_> = procs
            .iter()
            .filter(|p| tgid(p.pid).is_some_and(|leader| leader != p.pid))
            .map(|p| format!("{} ({})", p.name, p.pid))
            .collect();
        assert!(
            threads.is_empty(),
            "threads listed as processes: {threads:?}"
        );
    }
}
