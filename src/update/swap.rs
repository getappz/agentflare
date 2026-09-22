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
//!   the current binary aside (a uniquely named `.old.exe`) and copy the new one
//!   into place. If even the rename fails (the file is hard-locked), we fall
//!   back to a deferred `.bat` updater that finishes the swap once this process
//!   exits; [`SwapOutcome::Deferred`] tells the caller so it never claims success.
//!
//! [`replace_binary`] is the reusable primitive; `agentflare dev-install`
//! (item #127) calls it to install a freshly built binary over the installed
//! one without disturbing a running server.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// How [`replace_binary`] left `target`.
#[derive(Debug)]
pub(crate) enum SwapOutcome {
    /// The new binary is in place now.
    Installed,
    /// Windows only: `target` is hard-locked, so the copy was handed to a
    /// detached script that retries after this process exits. Until it succeeds
    /// the old binary is still installed; the script appends its result to `log`.
    #[cfg(windows)]
    Deferred { log: PathBuf },
}

/// Outcome of [`install_verified`]. Same shape as [`SwapOutcome`], but
/// `Installed` here means the swap *and* the byte-level verification both
/// passed -- callers can report success on this alone.
#[derive(Debug)]
pub(crate) enum InstallOutcome {
    Installed,
    #[cfg(windows)]
    Deferred {
        log: PathBuf,
    },
}

/// Why [`install_verified`] failed: at the swap step itself, or at the
/// post-swap verification. Callers use this to word their error differently
/// (a swap error and a "the install silently did nothing" error are not the
/// same failure).
pub(crate) enum InstallError {
    Swap(String),
    Verify(String),
}

/// [`replace_binary`], then confirm `target` actually ended up byte-identical
/// to `new_binary`. Never claim success on the swap's say-so alone: compare
/// what is on disk (item #624 -- a swap could report success, or pre-fix
/// silently swap nothing, while the installed bytes didn't match the build).
pub(crate) fn install_verified(
    new_binary: &Path,
    target: &Path,
) -> Result<InstallOutcome, InstallError> {
    match replace_binary(new_binary, target) {
        Ok(SwapOutcome::Installed) => {}
        #[cfg(windows)]
        Ok(SwapOutcome::Deferred { log }) => return Ok(InstallOutcome::Deferred { log }),
        Err(e) => return Err(InstallError::Swap(e)),
    }
    verify_installed(new_binary, target).map_err(InstallError::Verify)?;
    Ok(InstallOutcome::Installed)
}

/// Replace the binary at `target` with `new_binary`.
///
/// MCP-safe: never signals or kills any process. `Ok(Installed)` means the new
/// binary is in place; `Ok(Deferred)` (Windows locked-file fallback) means it is
/// only scheduled and callers must not report it as installed.
pub(crate) fn replace_binary(new_binary: &Path, target: &Path) -> Result<SwapOutcome, String> {
    #[cfg(windows)]
    {
        windows_replace(new_binary, target)
    }
    #[cfg(not(windows))]
    {
        unix_replace(new_binary, target)
    }
}

/// Confirm `target` is byte-identical to `built` (size, then SHA-256).
pub(crate) fn verify_installed(built: &Path, target: &Path) -> Result<(), String> {
    let (built_size, built_hash) = digest(built)?;
    let (target_size, target_hash) = digest(target)?;
    if (built_size, &built_hash) == (target_size, &target_hash) {
        return Ok(());
    }
    Err(format!(
        "installed file is {target_size} bytes (sha256 {}), the build is {built_size} bytes \
         (sha256 {})",
        &target_hash[..12],
        &built_hash[..12]
    ))
}

/// `(size in bytes, hex SHA-256)` of the file at `path`.
fn digest(path: &Path) -> Result<(u64, String), String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let size = std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok((size, hex::encode(hasher.finalize())))
}

/// Which processes might be holding a file a swap could not replace.
pub(crate) fn describe_holders() -> String {
    format_holders(&find_killable_pids())
}

fn format_holders(pids: &[u32]) -> String {
    if pids.is_empty() {
        return "no other agentflare process found; something else (antivirus, an indexer, \
                a shell) may be holding the file"
            .to_string();
    }
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "other agentflare processes still running (pid {list}); stop them (`agentflare daemon \
         stop`) and re-run"
    )
}

#[cfg(not(windows))]
fn unix_replace(new_binary: &Path, target: &Path) -> Result<SwapOutcome, String> {
    // Stage in the target's own directory so the final rename is a same-fs
    // atomic swap (a cross-device rename would fail with EXDEV).
    let staged = staging_path(target);
    std::fs::copy(new_binary, &staged).map_err(|e| format!("copy: {e}"))?;
    if let Err(e) = std::fs::rename(&staged, target) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("rename: {e}"));
    }
    Ok(SwapOutcome::Installed)
}

/// A pid-scoped sibling temp path in the target's directory, so concurrent
/// swaps against the same target can't clobber each other's staged file.
#[cfg(not(windows))]
fn staging_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.new", std::process::id()));
    target.with_file_name(name)
}

#[cfg(windows)]
fn windows_replace(new_binary: &Path, target: &Path) -> Result<SwapOutcome, String> {
    // Best-effort cleanup of earlier swaps' leftovers; any still running or
    // locked fail to delete and are left alone.
    remove_stale_asides(target);
    // Never reuse a fixed aside name: a leftover locked `agentflare.old.exe`
    // would make the rename below fail and wrongly force the deferred path.
    let old = unique_aside(target);

    // Renaming a running .exe is permitted on Windows and frees the target name.
    if std::fs::rename(target, &old).is_ok() {
        match std::fs::copy(new_binary, target) {
            Ok(_) => {
                // The old image may still be running; deleting it can fail — that
                // is harmless, the next swap cleans it up.
                let _ = std::fs::remove_file(&old);
                Ok(SwapOutcome::Installed)
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

/// A name for the aside copy of `target` that no earlier swap can have used
/// (pid + nanosecond timestamp), e.g. `agentflare.1234.1700000000000.old.exe`.
#[cfg(windows)]
fn unique_aside(target: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    target.with_extension(format!("{}.{nanos}.old.exe", std::process::id()))
}

/// Delete `<stem>.old.exe` / `<stem>.<anything>.old.exe` siblings of `target`.
/// Failures (still-running or locked images) are ignored: anything that will
/// not delete is left alone and retried by the next swap.
#[cfg(windows)]
fn remove_stale_asides(target: &Path) {
    let (Some(dir), Some(stem)) = (target.parent(), target.file_stem()) else {
        return;
    };
    let prefix = format!("{}.", stem.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && name.ends_with(".old.exe") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How many times the deferred script retries the copy before giving up.
#[cfg(windows)]
const DEFERRED_SWAP_ATTEMPTS: u32 = 10;

/// The deferred updater's batch script: wait for `pid` to exit, then copy
/// `staged` over `target`, retrying a bounded number of times and appending the
/// outcome to `log` (never discarding it).
#[cfg(windows)]
fn deferred_swap_script(pid: u32, staged: &Path, target: &Path, log: &Path) -> String {
    format!(
        "@echo off\r\n\
         set tries=0\r\n\
         :wait\r\n\
         tasklist /FI \"PID eq {pid}\" 2>nul | find \"{pid}\" >nul && (\r\n\
           ping -n 2 127.0.0.1 >nul\r\n\
           goto wait\r\n\
         )\r\n\
         :retry\r\n\
         copy /y \"{staged}\" \"{target}\" >nul 2>&1 && goto ok\r\n\
         set /a tries+=1\r\n\
         if %tries% GEQ {max} goto fail\r\n\
         ping -n 3 127.0.0.1 >nul\r\n\
         goto retry\r\n\
         :ok\r\n\
         echo %date% %time% swap ok: {target} >> \"{log}\"\r\n\
         del \"{staged}\" >nul 2>&1\r\n\
         goto done\r\n\
         :fail\r\n\
         echo %date% %time% swap FAILED after %tries% attempts, target still locked: {target} (staged copy kept at {staged}) >> \"{log}\"\r\n\
         :done\r\n\
         del \"%~f0\" >nul 2>&1\r\n",
        staged = staged.display(),
        target = target.display(),
        log = log.display(),
        max = DEFERRED_SWAP_ATTEMPTS,
    )
}

/// Write and launch a detached `.bat` that waits for this process to exit, then
/// copies the staged binary over `target`. Rustup uses the same trick for the
/// rare case where the running exe is locked against renaming.
#[cfg(windows)]
fn schedule_deferred_swap_windows(new_binary: &Path, target: &Path) -> Result<SwapOutcome, String> {
    let pid = std::process::id();
    let tmp = std::env::temp_dir();
    // Stage the new binary somewhere stable — the extraction tmpdir may be
    // cleaned before the deferred script runs.
    let staged = tmp.join(format!("agentflare-new-{pid}.exe"));
    std::fs::copy(new_binary, &staged).map_err(|e| format!("stage new binary: {e}"))?;
    let bat = tmp.join(format!("agentflare-swap-{pid}.bat"));
    let log = tmp.join("agentflare-swap.log");
    let script = deferred_swap_script(pid, &staged, target, &log);
    std::fs::write(&bat, script).map_err(|e| format!("write deferred updater: {e}"))?;
    // Hidden outer cmd; `start /B` detaches the swap script with no window
    // at all (`/min` would still pop a minimized one).
    flare_process::command("cmd")
        .args(["/C", "start", "/B", "", &bat.to_string_lossy()])
        .spawn()
        .map_err(|e| format!("spawn deferred updater: {e}"))?;
    Ok(SwapOutcome::Deferred { log })
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
    let output = flare_process::command("tasklist")
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

        let outcome =
            replace_binary(&new, &target).expect("swap should succeed for an unlocked file");
        assert!(matches!(outcome, SwapOutcome::Installed), "got {outcome:?}");
        assert_eq!(std::fs::read(&target).unwrap(), b"NEWCONTENT");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Open `path` with no sharing, so deleting or renaming over it fails with a
    /// sharing violation — the same symptom as an image another process maps.
    #[cfg(windows)]
    fn lock_exclusively(path: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .expect("exclusive open")
    }

    // Regression (item 624): a leftover, locked `agentflare.old.exe` used to make
    // the rename-aside fail, which silently fell into the deferred `.bat` path.
    #[cfg(windows)]
    #[test]
    fn locked_legacy_old_exe_does_not_force_the_deferred_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("agentflare.exe");
        let new = dir.path().join("new-binary");
        std::fs::write(&target, b"OLD").unwrap();
        std::fs::write(&new, b"NEWCONTENT").unwrap();
        let legacy = dir.path().join("agentflare.old.exe");
        std::fs::write(&legacy, b"ANCIENT").unwrap();
        let _lock = lock_exclusively(&legacy);
        assert!(std::fs::remove_file(&legacy).is_err(), "lock must hold");

        let outcome = replace_binary(&new, &target).expect("swap should succeed");

        assert!(matches!(outcome, SwapOutcome::Installed), "got {outcome:?}");
        assert_eq!(std::fs::read(&target).unwrap(), b"NEWCONTENT");
        assert!(legacy.exists(), "a file that will not delete is left alone");
    }

    #[cfg(windows)]
    #[test]
    fn stale_asides_are_removed_unless_locked() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("agentflare.exe");
        let new = dir.path().join("new-binary");
        std::fs::write(&target, b"OLD").unwrap();
        std::fs::write(&new, b"NEW").unwrap();
        let stale = dir.path().join("agentflare.111.222.old.exe");
        let locked = dir.path().join("agentflare.333.444.old.exe");
        let unrelated = dir.path().join("agentflare-shim.old.exe");
        for f in [&stale, &locked, &unrelated] {
            std::fs::write(f, b"x").unwrap();
        }
        let _lock = lock_exclusively(&locked);

        replace_binary(&new, &target).expect("swap should succeed");

        assert!(!stale.exists(), "unlocked stale aside is cleaned up");
        assert!(locked.exists(), "locked aside is left alone");
        assert!(unrelated.exists(), "other binaries' asides are not touched");
    }

    #[cfg(windows)]
    #[test]
    fn unique_aside_never_repeats_or_collides_with_the_legacy_name() {
        let target = Path::new(r"C:\bin\agentflare.exe");
        let (a, b) = (unique_aside(target), unique_aside(target));
        assert_ne!(a, b);
        assert_ne!(a, target.with_extension("old.exe"));
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("agentflare."), "got {name}");
        assert!(name.ends_with(".old.exe"), "got {name}");
    }

    #[test]
    fn verify_installed_accepts_identical_and_rejects_any_difference() {
        let dir = tempfile::tempdir().unwrap();
        let built = dir.path().join("built");
        let same = dir.path().join("same");
        let shorter = dir.path().join("shorter");
        let same_size = dir.path().join("same-size");
        std::fs::write(&built, b"NEWCONTENT").unwrap();
        std::fs::write(&same, b"NEWCONTENT").unwrap();
        std::fs::write(&shorter, b"NEW").unwrap();
        std::fs::write(&same_size, b"OLDCONTENT").unwrap();

        assert!(verify_installed(&built, &same).is_ok());
        // A stale install (the item 624 symptom) must be a failure, not a success.
        let e = verify_installed(&built, &shorter).unwrap_err();
        assert!(e.contains("3 bytes") && e.contains("10 bytes"), "got {e}");
        assert!(verify_installed(&built, &same_size).is_err());
        assert!(verify_installed(&built, &dir.path().join("missing")).is_err());
    }

    #[test]
    fn format_holders_lists_pids_or_says_none_found() {
        assert!(format_holders(&[7, 9]).contains("pid 7, 9"));
        assert!(format_holders(&[]).contains("no other agentflare process"));
    }

    // Regression coverage for the `update` half of item 624/627: this is the
    // exact call `update::run` now makes on `Ok(SwapOutcome::Installed)`. The
    // happy path lands verified bytes; a target that diverges afterward
    // (the item-624 symptom of a swap claiming success while the installed
    // bytes don't match) must be caught by the same check dev_install uses,
    // not silently reported as installed.
    #[test]
    fn install_verified_verifies_the_swap_update_run_performs() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(if cfg!(windows) {
            "agentflare.exe"
        } else {
            "agentflare"
        });
        let new = dir.path().join("new-binary");
        std::fs::write(&target, b"OLD").unwrap();
        std::fs::write(&new, b"NEWCONTENT").unwrap();

        assert!(matches!(
            install_verified(&new, &target),
            Ok(InstallOutcome::Installed)
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"NEWCONTENT");

        // A build whose installed bytes are made to differ afterward must be
        // caught, not silently accepted as installed.
        std::fs::write(&target, b"TAMPERED").unwrap();
        let e = verify_installed(&new, &target).unwrap_err();
        assert!(e.contains("8 bytes") && e.contains("10 bytes"), "got {e}");
    }

    #[cfg(windows)]
    #[test]
    fn deferred_script_retries_a_bounded_number_of_times_and_logs() {
        let script = deferred_swap_script(
            42,
            Path::new(r"C:\tmp\new.exe"),
            Path::new(r"C:\bin\agentflare.exe"),
            Path::new(r"C:\tmp\agentflare-swap.log"),
        );
        assert!(script.contains(&format!("GEQ {DEFERRED_SWAP_ATTEMPTS} goto fail")));
        assert!(script.contains("goto retry"));
        assert!(script.contains(r#">> "C:\tmp\agentflare-swap.log""#));
        assert!(script.contains("swap FAILED"));
        assert!(script.contains("swap ok"));
        // Success is decided by the copy's exit code, not assumed.
        assert!(script.contains(r#""C:\bin\agentflare.exe" >nul 2>&1 && goto ok"#));
    }
}
