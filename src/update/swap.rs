//! MCP-safe self-replacement of the `agentflare` binary.
//!
//! Replacing the running binary must not break a live `agentflare mcp` stdio
//! server. This is safe by construction: a running process keeps executing its
//! already-loaded image, so a swap that only touches the *file on disk* never
//! disturbs a live server — it simply picks up the new binary on next launch.
//! We therefore never signal or kill any process to perform a swap.
//!
//! - **Unix**: copy alongside the target, then `rename()` over it (atomic on the
//!   same filesystem). The live process keeps its original inode.
//! - **Windows**: a running `.exe` can be *renamed* but not deleted, so we move
//!   the current binary aside (`.old.exe`) and copy the new one into place. If
//!   even the rename fails (the file is hard-locked), we fall back to a deferred
//!   `.bat` updater that finishes the swap once this process exits.
//!
//! [`replace_binary`] is the reusable primitive; `agentflare dev-install`
//! (item #127) calls it to install a freshly built binary over the installed
//! one without disturbing a running server.

use std::path::Path;

/// Replace the binary at `target` with `new_binary`.
///
/// MCP-safe: never signals or kills any process. On success the new binary is
/// in place (or, on the Windows locked-file fallback, scheduled to be put in
/// place the moment this process exits).
pub(crate) fn replace_binary(new_binary: &Path, target: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows_replace(new_binary, target)
    }
    #[cfg(not(windows))]
    {
        unix_replace(new_binary, target)
    }
}

#[cfg(not(windows))]
fn unix_replace(new_binary: &Path, target: &Path) -> Result<(), String> {
    // Stage in the target's own directory so the final rename is a same-fs
    // atomic swap (a cross-device rename would fail with EXDEV).
    let staged = staging_path(target);
    std::fs::copy(new_binary, &staged).map_err(|e| format!("copy: {e}"))?;
    if let Err(e) = std::fs::rename(&staged, target) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("rename: {e}"));
    }
    Ok(())
}

/// A pid-scoped sibling temp path in the target's directory, so concurrent
/// swaps against the same target can't clobber each other's staged file.
#[cfg(not(windows))]
fn staging_path(target: &Path) -> std::path::PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.new", std::process::id()));
    target.with_file_name(name)
}

#[cfg(windows)]
fn windows_replace(new_binary: &Path, target: &Path) -> Result<(), String> {
    let old = target.with_extension("old.exe");
    // Best-effort cleanup of a previous swap's leftover (it may still be a
    // running image from an earlier update, in which case the delete no-ops).
    let _ = std::fs::remove_file(&old);

    // Renaming a running .exe is permitted on Windows and frees the target name.
    if std::fs::rename(target, &old).is_ok() {
        match std::fs::copy(new_binary, target) {
            Ok(_) => {
                // The old image may still be running; deleting it can fail — that
                // is harmless, the next swap cleans it up.
                let _ = std::fs::remove_file(&old);
                Ok(())
            }
            Err(e) => {
                // Never leave the install without a binary: put the old one back.
                if let Err(re) = std::fs::rename(&old, target) {
                    return Err(format!(
                        "copy new binary failed ({e}); rollback also failed ({re}); \
                         previous binary preserved at {}",
                        old.display()
                    ));
                }
                Err(format!("copy new binary: {e}"))
            }
        }
    } else {
        // The file is hard-locked and cannot even be renamed. Defer the swap to
        // a batch script that runs after this process exits.
        schedule_deferred_swap_windows(new_binary, target)
    }
}

/// Write and launch a detached `.bat` that waits for this process to exit, then
/// copies the staged binary over `target`. Rustup uses the same trick for the
/// rare case where the running exe is locked against renaming.
#[cfg(windows)]
fn schedule_deferred_swap_windows(new_binary: &Path, target: &Path) -> Result<(), String> {
    let pid = std::process::id();
    let tmp = std::env::temp_dir();
    // Stage the new binary somewhere stable — the extraction tmpdir may be
    // cleaned before the deferred script runs.
    let staged = tmp.join(format!("agentflare-new-{pid}.exe"));
    std::fs::copy(new_binary, &staged).map_err(|e| format!("stage new binary: {e}"))?;
    let bat = tmp.join(format!("agentflare-swap-{pid}.bat"));
    let script = format!(
        "@echo off\r\n\
         :wait\r\n\
         tasklist /FI \"PID eq {pid}\" 2>nul | find \"{pid}\" >nul && (\r\n\
           ping -n 2 127.0.0.1 >nul\r\n\
           goto wait\r\n\
         )\r\n\
         copy /y \"{staged}\" \"{target}\" >nul\r\n\
         del \"{staged}\" >nul 2>&1\r\n\
         del \"%~f0\" >nul 2>&1\r\n",
        pid = pid,
        staged = staged.display(),
        target = target.display(),
    );
    std::fs::write(&bat, script).map_err(|e| format!("write deferred updater: {e}"))?;
    std::process::Command::new("cmd")
        .args(["/C", "start", "/min", "", &bat.to_string_lossy()])
        .spawn()
        .map_err(|e| format!("spawn deferred updater: {e}"))?;
    Ok(())
}

/// PIDs of *other* running `agentflare` processes, excluding this process.
///
/// Reported after an install so the user knows which instances need a restart
/// to pick up the new binary. Deliberately **not** used to kill anything: a
/// live `agentflare mcp` server keeps running its loaded image safely, and
/// killing it would break the very session performing the update.
pub(crate) fn find_killable_pids() -> Vec<u32> {
    let self_pid = std::process::id();
    let raw = list_agentflare_pids_raw();
    parse_other_pids(&raw, self_pid)
}

/// Shell out to the platform process lister and return its raw stdout. Kept
/// separate from [`parse_other_pids`] so the parsing is unit-testable.
fn list_agentflare_pids_raw() -> String {
    #[cfg(windows)]
    let output = std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq agentflare.exe", "/FO", "CSV", "/NH"])
        .output();
    #[cfg(not(windows))]
    let output = std::process::Command::new("pgrep")
        .args(["-x", "agentflare"])
        .output();

    output
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Extract PIDs from a process-lister's stdout, dropping `self_pid`.
///
/// Handles both `pgrep -x` output (one PID per line) and `tasklist /FO CSV /NH`
/// output (`"agentflare.exe","1234",...`) by scanning each line for the first
/// integer field.
fn parse_other_pids(raw: &str, self_pid: u32) -> Vec<u32> {
    let mut pids = Vec::new();
    for line in raw.lines() {
        // `pgrep`: the whole line is the PID. `tasklist` CSV: the PID is the
        // second quoted field, e.g. `"agentflare.exe","1234","Console",...`.
        let candidate = line
            .split(&['"', ',', ' ', '\t'][..])
            .find_map(|tok| tok.trim().parse::<u32>().ok());
        if let Some(pid) = candidate
            && pid != self_pid
            && !pids.contains(&pid)
        {
            pids.push(pid);
        }
    }
    pids
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(not(windows))]
    #[test]
    fn staging_path_is_a_pid_scoped_sibling_of_the_target() {
        let target = Path::new("/opt/bin/agentflare");
        let staged = staging_path(target);
        assert_eq!(staged.parent(), target.parent());
        let name = staged.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("agentflare."), "got {name}");
        assert!(name.ends_with(".new"), "got {name}");
        assert!(name.contains(&std::process::id().to_string()), "got {name}");
    }

    #[test]
    fn parse_other_pids_reads_pgrep_lines_and_drops_self() {
        let raw = "111\n222\n333\n";
        assert_eq!(parse_other_pids(raw, 222), vec![111, 333]);
    }

    #[test]
    fn parse_other_pids_reads_tasklist_csv() {
        let raw = "\"agentflare.exe\",\"111\",\"Console\",\"1\",\"12,345 K\"\n\
                   \"agentflare.exe\",\"222\",\"Console\",\"1\",\"12,345 K\"\n";
        // 222 is self; the "12,345 K" memory column must not be mis-read as a PID
        // because the first integer field on each line is the PID.
        assert_eq!(parse_other_pids(raw, 222), vec![111]);
    }

    #[test]
    fn parse_other_pids_dedups() {
        assert_eq!(parse_other_pids("111\n111\n", 999), vec![111]);
    }

    #[test]
    fn parse_other_pids_ignores_blank_and_headerish_lines() {
        assert_eq!(parse_other_pids("\nINFO: No tasks\n444\n", 1), vec![444]);
    }

    // The actual swap is exercised on the host platform: copy a fake "new"
    // binary over a "target" and confirm the bytes land. On Windows this drives
    // the rename+copy path; on Unix the stage+rename path.
    #[test]
    fn replace_binary_puts_new_bytes_in_place() {
        let dir =
            std::env::temp_dir().join(format!("agentflare-swap-test-{}-rb", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let target = dir.join(if cfg!(windows) {
            "agentflare.exe"
        } else {
            "agentflare"
        });
        let new = dir.join("new-binary");

        {
            let mut f = std::fs::File::create(&target).unwrap();
            f.write_all(b"OLD").unwrap();
        }
        {
            let mut f = std::fs::File::create(&new).unwrap();
            f.write_all(b"NEWCONTENT").unwrap();
        }

        replace_binary(&new, &target).expect("swap should succeed for an unlocked file");
        assert_eq!(std::fs::read(&target).unwrap(), b"NEWCONTENT");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
