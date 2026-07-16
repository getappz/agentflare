//! `agentflare dev-install` — build the current source tree and atomically
//! install it over the running binary.
//!
//! Reuses the MCP-safe swap from [`crate::update::swap`] (item #122): the swap
//! never kills any process, so running `dev-install` from your installed
//! `agentflare` while an `agentflare mcp` server is live does not break the
//! server — it picks up the new binary on next launch.

mod cargo;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// How long to wait for the freshly built binary to answer `--version` before
/// declaring the build broken. `--version` returns immediately; this only
/// guards a pathological hang.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(15);

/// Build (release unless `!release`), verify, and replace the running binary.
pub fn run(release: bool, dry_run: bool) {
    println!(
        "building agentflare ({})...",
        if release { "release" } else { "debug" }
    );
    let built = match cargo::build_and_locate(release) {
        Ok(p) if p.exists() => p,
        Ok(p) => {
            eprintln!(
                "error: cargo reported {} but it does not exist",
                p.display()
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    // Verify the fresh build runs *before* replacing anything, so a broken
    // build never overwrites a working install.
    if let Err(e) = verify_runs(&built) {
        eprintln!("error: built binary failed verification: {e}");
        std::process::exit(1);
    }

    let target = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot determine current binary path: {e}");
            std::process::exit(1);
        }
    };

    if same_file(&built, &target) {
        eprintln!(
            "refusing to install over the build output itself ({}).\n\
             Run `dev-install` from your *installed* agentflare, not the freshly built binary.",
            target.display()
        );
        std::process::exit(1);
    }

    if dry_run {
        println!(
            "dry-run: would install {} -> {}",
            built.display(),
            target.display()
        );
        return;
    }

    println!("installing {} -> {}", built.display(), target.display());
    if let Err(e) = crate::update::swap::replace_binary(&built, &target) {
        eprintln!("error installing binary: {e}");
        std::process::exit(1);
    }
    println!("installed to {}", target.display());
    println!("run `agentflare --version` to confirm");
}

/// Run `<binary> --version` and confirm it exits successfully within
/// [`VERIFY_TIMEOUT`].
fn verify_runs(binary: &Path) -> Result<(), String> {
    let mut child = Command::new(binary)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn --version: {e}"))?;

    let deadline = Instant::now() + VERIFY_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("--version exited with {status}")),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err("--version timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("waiting on --version: {e}")),
        }
    }
}

/// Whether two paths resolve to the same file. Canonicalizes both (following
/// symlinks); falls back to a raw comparison when a path can't be canonicalized
/// (e.g. the target doesn't exist yet).
fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_file_true_for_identical_path_false_for_distinct() {
        let dir =
            std::env::temp_dir().join(format!("agentflare-devinstall-same-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("bin");
        std::fs::write(&f, b"x").unwrap();
        let other = dir.join("other");
        std::fs::write(&other, b"y").unwrap();

        assert!(same_file(&f, &f));
        assert!(!same_file(&f, &other));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_runs_errors_for_a_missing_binary() {
        // The happy path is exercised by the real `dev-install` flow against a
        // freshly built binary; here we pin down the guard that a non-runnable
        // path is reported as an error rather than panicking.
        let missing = std::env::temp_dir().join("agentflare-nonexistent-binary-xyz");
        assert!(verify_runs(&missing).is_err());
    }
}
