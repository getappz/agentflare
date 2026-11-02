use super::run_tether;
use super::tether::worktree_removal_detected;
use std::ffi::OsStr;
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

#[cfg(windows)]
fn assert_no_surviving_process(image_name: &str, cmdline_needle: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let ps_query = format!(
            "(Get-CimInstance Win32_Process -Filter \"Name='{image_name}'\" | \
             Where-Object {{ $_.CommandLine -like '*{cmdline_needle}*' }} | \
             Select-Object -ExpandProperty ProcessId) -join ','"
        );
        let out = Command::new("powershell")
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
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    let root_rm = root.clone();
    #[cfg(windows)]
    let (program, args): (&OsStr, Vec<&OsStr>) = (
        OsStr::new("ping"),
        vec![OsStr::new("-n"), OsStr::new("120"), OsStr::new("127.0.0.1")],
    );
    #[cfg(unix)]
    let (program, args): (&OsStr, Vec<&OsStr>) = (
        OsStr::new("sleep"),
        vec![OsStr::new("120")],
    );
    let handle = std::thread::spawn(move || run_tether(&root, program, &args));
    std::thread::sleep(Duration::from_millis(800));
    let _ = std::fs::remove_dir_all(&root_rm);
    let result = handle.join().expect("tether thread panicked");
    assert!(result.is_ok(), "tether returned error: {result:?}");
    #[cfg(windows)]
    assert_no_surviving_process("ping.exe", "127.0.0.1");
}
