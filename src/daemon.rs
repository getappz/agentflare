use fs2::FileExt;
use std::path::PathBuf;
use std::time::Duration;

use crate::ipc::{DaemonAddr, process};

pub fn daemon_pid_path() -> PathBuf {
    dirs::runtime_dir()
        .map(|d| d.join("agentflare").join("daemon.pid"))
        .unwrap_or_else(|| std::env::temp_dir().join("agentflare-daemon.pid"))
}

/// stdout+stderr of the current daemon session — truncated fresh on every
/// `start`/`restart` (see `spawn_detached`), same lifetime as the runtime
/// dir it lives in. Not a rotated history: `agentflare daemon logs` is for
/// seeing what THIS run of the daemon is doing, not an audit trail.
pub fn daemon_log_path() -> PathBuf {
    dirs::runtime_dir()
        .map(|d| d.join("agentflare").join("daemon.log"))
        .unwrap_or_else(|| std::env::temp_dir().join("agentflare-daemon.log"))
}

pub fn daemon_start_lock_path() -> PathBuf {
    dirs::runtime_dir()
        .map(|d| d.join("agentflare").join("daemon.start.lock"))
        .unwrap_or_else(|| std::env::temp_dir().join("agentflare-daemon.start.lock"))
}

fn daemon_singleton_lock_path() -> PathBuf {
    dirs::runtime_dir()
        .map(|d| d.join("agentflare").join("daemon.singleton.lock"))
        .unwrap_or_else(|| std::env::temp_dir().join("agentflare-daemon.singleton.lock"))
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
    acquire_lock(daemon_start_lock_path(), Duration::from_secs(5))
}

/// Serializes the is_daemon_running-check + write_pid_file critical section
/// in `ServeArgs::run()`, so two `agentflare serve` invocations racing each
/// other (e.g. on different `--port`s, started within moments of each other)
/// can't both pass the check before either has written its pid file.
///
/// Deliberately a *different* lock file from `daemon_start_lock_path()`:
/// `start_daemon()` holds that lock for its whole ~5s spawn-and-poll window,
/// and the process it spawns calls back into this same check via
/// `serve --_foreground-daemon` — sharing one lock would deadlock the two
/// (parent waiting on the child's pid file while holding the lock the child
/// needs to write it).
pub fn acquire_singleton_lock() -> Result<LockGuard, String> {
    acquire_lock(daemon_singleton_lock_path(), Duration::from_secs(2))
}

fn acquire_lock(lock_path: PathBuf, timeout: Duration) -> Result<LockGuard, String> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create lock dir {parent:?}: {e}"))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("open lock file: {e}"))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if file.try_lock_exclusive().is_ok() {
            return Ok(LockGuard {
                _file: file,
                path: lock_path,
            });
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("could not acquire lock within {timeout:?}"));
        }
        std::thread::sleep(Duration::from_millis(50));
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

/// Records the calling process's own pid as the running daemon, so a later
/// `is_daemon_running`/`daemon status`/`daemon stop` (possibly from a
/// different process) can find it. Called by the `serve` foreground process
/// itself once it starts, not just by `start_daemon`'s spawner.
pub fn write_pid_file() -> Result<(), String> {
    let path = daemon_pid_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create pid dir {parent:?}: {e}"))?;
    }
    std::fs::write(&path, std::process::id().to_string())
        .map_err(|e| format!("write pid file: {e}"))
}

pub fn start_daemon() -> Result<u32, String> {
    let _guard = acquire_start_lock()?;

    if let Some(pid) = is_daemon_running() {
        return Ok(pid);
    }

    let binary = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let log_path = daemon_log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create log dir {parent:?}: {e}"))?;
    }
    // Must match the `ExecStart`/`ProgramArguments` invocation the installed
    // systemd/launchd units use (see `daemon_autostart.rs`) — both spawn
    // `serve --_foreground-daemon`, not just the bare flag.
    let _pid = process::spawn_detached(
        &binary.to_string_lossy(),
        &["serve", "--_foreground-daemon"],
        Some(&log_path),
    )?;

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

/// A point-in-time fingerprint (mtime + size) of the on-disk binary this
/// process was launched from. `stat()`-cheap, and a genuinely different
/// build sharing both by chance is astronomically unlikely -- no need for a
/// content hash re-read on every check.
///
/// Exists because work items run in-process via `WorkItemExecutor` (item
/// #19) instead of a spawned subprocess: a rebuild/`agentflare update` that
/// replaces the binary no longer makes dispatch fail loudly (the old
/// `dispatch_item` fix in `supervisor.rs`'s git history is moot once
/// nothing spawns an exe path) -- it leaves a long-running daemon silently
/// executing whatever job logic it happened to load at startup, forever
/// (item #68).
pub struct BinarySnapshot {
    path: PathBuf,
    modified: std::time::SystemTime,
    len: u64,
}

impl BinarySnapshot {
    /// `None` if `current_exe()` fails or the resolved path can't be
    /// stat'd -- the staleness watchdog then has nothing to compare against
    /// and skips itself, same tradeoff as any other best-effort startup
    /// check in this module.
    pub fn capture() -> Option<Self> {
        let path = std::env::current_exe().ok()?;
        let meta = std::fs::metadata(&path).ok()?;
        Some(Self {
            path,
            modified: meta.modified().ok()?,
            len: meta.len(),
        })
    }

    /// `true` once the file now on disk at `path` no longer matches what
    /// this process was actually loaded from -- e.g. `agentflare update`'s
    /// same-fs `rename()` (see `update::swap`'s doc comment: the live
    /// process keeps its original, now-unlinked inode) or a `cargo
    /// build`/`cargo install` replaced the file while this process kept
    /// running the old image.
    pub fn is_stale(&self) -> bool {
        match std::fs::metadata(&self.path).and_then(|m| Ok((m.modified()?, m.len()))) {
            Ok((modified, len)) => modified != self.modified || len != self.len,
            // Gone entirely (e.g. uninstalled) counts as stale too --
            // nothing to keep serving from.
            Err(_) => true,
        }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// Polls `snapshot.is_stale()` every `interval`, returning as soon as it's
/// `true`. Split out from `dashboard::server`'s `spawn_binary_staleness_watchdog`
/// so the polling *task* itself -- not just `is_stale()`'s comparison logic
/// in isolation -- has test coverage (item #107: the watchdog silently never
/// fired in a live daemon despite `is_stale()`'s own unit tests passing,
/// which unit tests on `is_stale()` alone couldn't have caught).
pub async fn wait_for_stale(snapshot: &BinarySnapshot, interval: Duration) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        ticker.tick().await;
        if snapshot.is_stale() {
            return;
        }
    }
}

/// Spawns a replacement daemon from the (now-updated) binary at
/// `snapshot`'s path and exits this process. Cleans up this process's own
/// pid/lock files *first* so the replacement's own `is_daemon_running()`
/// gate (`ServeArgs::run`) can't mistake this still-exiting process for a
/// still-live one and refuse to start.
///
/// Deliberately does *not* wait for any in-process job the `WorkerPool` may
/// have running on another thread right now. That looks like it could
/// reintroduce the exact silent-failure shape this item exists to close --
/// but it doesn't, because it's not a new risk: `dashboard::server`'s
/// `reconcile_orphaned_jobs`, which the replacement daemon runs before its
/// own `WorkerPool::start` (see that function's doc comment for why the
/// order matters), already exists to make an abruptly-dead daemon's
/// in-flight jobs fail loud and release their claim rather than vanish --
/// built for the ordinary crash/kill -9/OOM case (item #40), and this exit
/// is just a self-triggered instance of that same case. The one real
/// difference -- durable side effects a job already committed (a merged
/// commit, an opened PR) land before this process's own job-row bookkeeping
/// does -- means the worst case here is a stray "orphaned" comment on an
/// item that actually succeeded, not a silently-uncommitted, no-PR item
/// wrongly marked done (item #66/PR #436's failure mode). Waiting instead
/// would trade a bounded, already-handled cosmetic gap for an unbounded one:
/// exactly the indefinite stale-logic window this watchdog exists to close.
pub fn respawn_from_stale(snapshot: &BinarySnapshot) -> ! {
    cleanup_daemon_files();
    let binary = snapshot.path().to_string_lossy().to_string();
    // Must match `start_daemon`'s own spawn call -- both need the installed
    // systemd/launchd units' `serve --_foreground-daemon` invocation.
    match process::spawn_detached(
        &binary,
        &["serve", "--_foreground-daemon"],
        Some(&daemon_log_path()),
    ) {
        Ok(pid) => eprintln!("agentflare-daemon: respawned as pid {pid}"),
        Err(e) => eprintln!(
            "agentflare-daemon: failed to respawn ({e}); exiting anyway -- a process \
             supervisor (systemd/launchd), if configured, should restart it"
        ),
    }
    std::process::exit(1)
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

#[cfg(test)]
mod binary_snapshot_tests {
    use super::BinarySnapshot;
    use std::io::Write;

    fn snapshot_at(path: &std::path::Path) -> BinarySnapshot {
        let meta = std::fs::metadata(path).unwrap();
        BinarySnapshot {
            path: path.to_path_buf(),
            modified: meta.modified().unwrap(),
            len: meta.len(),
        }
    }

    #[test]
    fn is_stale_is_false_right_after_capture() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agentflare");
        std::fs::write(&path, b"v1").unwrap();

        let snapshot = snapshot_at(&path);
        assert!(!snapshot.is_stale());
    }

    #[test]
    fn is_stale_is_true_once_the_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agentflare");
        std::fs::write(&path, b"v1").unwrap();
        let snapshot = snapshot_at(&path);

        // Same replace-by-rename shape as `update::swap::replace_binary`:
        // a longer file forces a distinct size even if two builds landed
        // in the same mtime second.
        let staged = dir.path().join("agentflare.new");
        {
            let mut f = std::fs::File::create(&staged).unwrap();
            f.write_all(b"v2-longer-content").unwrap();
        }
        std::fs::rename(&staged, &path).unwrap();

        assert!(snapshot.is_stale());
    }

    #[test]
    fn is_stale_is_true_once_the_file_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agentflare");
        std::fs::write(&path, b"v1").unwrap();
        let snapshot = snapshot_at(&path);

        std::fs::remove_file(&path).unwrap();

        assert!(snapshot.is_stale());
    }

    // Regression coverage for item #107: `is_stale()` itself was already
    // correct (the three tests above), but the live daemon's watchdog task
    // never fired anyway. This exercises `wait_for_stale`'s actual polling
    // loop -- not just the comparison it polls -- to catch that class of bug.
    #[tokio::test(start_paused = true)]
    async fn wait_for_stale_polls_until_the_file_on_disk_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agentflare");
        std::fs::write(&path, b"v1").unwrap();
        let snapshot = snapshot_at(&path);
        let interval = std::time::Duration::from_secs(60);

        let handle = tokio::spawn(async move {
            super::wait_for_stale(&snapshot, interval).await;
        });

        // `interval`'s first tick fires immediately; let that first (not
        // stale) check happen before advancing the clock.
        tokio::task::yield_now().await;
        assert!(!handle.is_finished());

        tokio::time::advance(interval / 2).await;
        assert!(!handle.is_finished(), "fired before the file changed");

        let staged = dir.path().join("agentflare.new");
        std::fs::write(&staged, b"v2-longer-content").unwrap();
        std::fs::rename(&staged, &path).unwrap();

        tokio::time::advance(interval).await;
        tokio::task::yield_now().await;

        assert!(
            handle.is_finished(),
            "wait_for_stale never returned after the on-disk binary changed"
        );
    }
}
