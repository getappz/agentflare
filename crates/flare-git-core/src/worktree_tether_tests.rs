use super::run_tether;
use super::tether::worktree_removal_detected;
use std::ffi::OsStr;
use std::time::Duration;
use tempfile::TempDir;

/// The pending-signal flag is process-wide, so tests that run a tether must not overlap.
static TETHER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(windows)]
fn assert_no_surviving_process(image_name: &str, cmdline_needle: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let ps_query = format!(
            "(Get-CimInstance Win32_Process -Filter \"Name='{image_name}'\" | \
             Where-Object {{ $_.CommandLine -like '*{cmdline_needle}*' }} | \
             Select-Object -ExpandProperty ProcessId) -join ','"
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps_query])
            .output();
        let survivors = out
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if survivors.is_empty() {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "tether left {image_name} matching '{cmdline_needle}' running (pid(s): {survivors})"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn worktree_removal_detected_only_on_not_found() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(worktree_removal_detected(tmp.path()), Some(false));
    let missing = tmp.path().join("gone");
    assert_eq!(worktree_removal_detected(&missing), Some(true));
}

#[test]
fn tether_spawn_failure_is_reported() {
    let tmp = TempDir::new().unwrap();
    let err = run_tether(
        tmp.path(),
        OsStr::new("definitely-not-a-real-binary-agentflare-tether"),
        &[],
    )
    .unwrap_err();
    assert!(err.contains("spawn"), "expected spawn error, got: {err}");
}

#[test]
fn tether_kills_child_when_worktree_root_removed() {
    let _serial = TETHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    // A worktree root has a `.git` entry; on Windows the child's cwd keeps the emptied root alive.
    std::fs::write(
        root.join(".git"),
        "gitdir: elsewhere
",
    )
    .unwrap();
    let root_rm = root.clone();
    #[cfg(windows)]
    let (program, args): (&OsStr, Vec<&OsStr>) = (
        OsStr::new("ping"),
        vec![OsStr::new("-n"), OsStr::new("120"), OsStr::new("127.0.0.1")],
    );
    #[cfg(unix)]
    let (program, args): (&OsStr, Vec<&OsStr>) = (OsStr::new("sleep"), vec![OsStr::new("120")]);
    let started = std::time::Instant::now();
    let handle = std::thread::spawn(move || run_tether(&root, program, &args));
    std::thread::sleep(Duration::from_millis(800));
    let _ = std::fs::remove_dir_all(&root_rm);
    let result = handle.join().expect("tether thread panicked");
    // The child sleeps for 120 s: returning early proves it was killed, not outlived.
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "child was not killed: {:?}",
        started.elapsed()
    );
    assert!(result.is_ok(), "tether returned error: {result:?}");
    #[cfg(windows)]
    assert_no_surviving_process("ping.exe", "127.0.0.1");
}

/// SIGINT/SIGHUP to the tether must kill the child *and* its grandchild (#827).
#[cfg(unix)]
#[test]
fn tether_signal_kills_child_and_grandchild() {
    for sig in [libc::SIGINT, libc::SIGHUP] {
        let _serial = TETHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let pidfile = root.join("grandchild.pid");
        let script = "sleep 120 & echo $! > grandchild.pid; wait";
        let started = std::time::Instant::now();
        let handle = std::thread::spawn(move || {
            run_tether(
                &root,
                OsStr::new("sh"),
                &[OsStr::new("-c"), OsStr::new(script)],
            )
        });
        // Long enough for the child to spawn and for the tether to install its handlers.
        std::thread::sleep(Duration::from_millis(1500));
        let grandchild: i32 = std::fs::read_to_string(&pidfile)
            .expect("grandchild pid recorded")
            .trim()
            .parse()
            .unwrap();
        // SAFETY: raising a signal the tether handler is installed for.
        unsafe { libc::raise(sig) };
        let result = handle.join().expect("tether thread panicked");
        assert_eq!(result, Ok(128 + sig), "signal {sig}");
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "child outlived the signal"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        // SAFETY: signal 0 only probes for existence.
        while unsafe { libc::kill(grandchild, 0) } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "grandchild {grandchild} survived signal {sig}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// A console control event must kill the child tree. The handler is invoked directly:
/// `GenerateConsoleCtrlEvent` would also hit cargo and the test harness sharing this console.
#[cfg(windows)]
#[test]
fn tether_console_ctrl_event_kills_child_tree() {
    let _serial = TETHER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let args = [OsStr::new("-n"), OsStr::new("120"), OsStr::new("127.0.0.1")];
    let started = std::time::Instant::now();
    let handle = std::thread::spawn(move || run_tether(&root, OsStr::new("ping"), &args));
    std::thread::sleep(Duration::from_millis(1500));
    // SAFETY: CTRL_C_EVENT (0) takes the non-blocking path of the handler.
    assert_eq!(unsafe { super::tether_signal::on_ctrl(0) }, 1);
    let result = handle.join().expect("tether thread panicked");
    assert_eq!(result, Ok(130));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "child outlived the event"
    );
    assert_no_surviving_process("ping.exe", "127.0.0.1");
}
