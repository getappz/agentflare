//! `agentflare update` — self-upgrade from GitHub releases.
//!
//! Split into submodules:
//! - [`github`] — release discovery, download, checksum verification.
//! - [`swap`] — MCP-safe binary replacement (reused by `dev-install`, item #127).

mod github;
pub(crate) mod swap;

/// Download, verify, and install the requested (or latest) release.
pub fn run(version: Option<String>, check_only: bool, quiet: bool) {
    let target_version = match resolve_target_version(version, check_only, quiet) {
        Some(v) => v,
        None => return,
    };

    let asset = github::asset_name(&target_version);

    if !quiet {
        println!("downloading {asset}...");
    }
    let data = match github::download_asset(&target_version, &asset) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error downloading: {e}");
            std::process::exit(1);
        }
    };

    if !quiet {
        println!("verifying checksum...");
    }
    match github::expected_checksum(&target_version, &asset) {
        Ok(expected) => {
            if let Err(e) = github::verify_checksum(&data, &expected) {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("error fetching checksum: {e}");
            std::process::exit(1);
        }
    }

    let new_binary = match extract_binary(&data) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    let current = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: cannot determine current binary path: {e}");
            std::process::exit(1);
        }
    };

    if !quiet {
        println!("replacing {}...", current.display());
    }
    if let Err(e) = swap::replace_binary(&new_binary, &current) {
        eprintln!("error replacing binary: {e}");
        std::process::exit(1);
    }

    if !quiet {
        println!("updated to {target_version}");
        report_other_instances();
        println!("run `agentflare init --agent <agent>` to rewire hooks if needed");
    }
}

/// Resolve which version to install, or `None` when there is nothing to do
/// (already up to date, or `--check` only).
fn resolve_target_version(
    version: Option<String>,
    check_only: bool,
    quiet: bool,
) -> Option<String> {
    match version {
        Some(v) => {
            let v = if v.starts_with('v') {
                v
            } else {
                format!("v{v}")
            };
            if !quiet {
                println!("updating to {v}...");
            }
            Some(v)
        }
        None => {
            let latest = match github::check_for_update() {
                Ok(Some(v)) => v,
                Ok(None) => {
                    println!("agentflare is up to date (v{})", env!("CARGO_PKG_VERSION"));
                    return None;
                }
                Err(e) => {
                    eprintln!("error checking for updates: {e}");
                    std::process::exit(1);
                }
            };
            if check_only {
                if !quiet {
                    println!(
                        "new version available: {latest} (current: v{})",
                        env!("CARGO_PKG_VERSION")
                    );
                }
                return None;
            }
            if !quiet {
                println!("updating to {latest}...");
            }
            Some(latest)
        }
    }
}

/// Extract the release archive into a per-pid tmpdir and return the path to the
/// unpacked binary.
fn extract_binary(data: &[u8]) -> Result<std::path::PathBuf, String> {
    let tmpdir = std::env::temp_dir().join(format!("agentflare-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmpdir);
    std::fs::create_dir_all(&tmpdir).map_err(|e| format!("create tmpdir: {e}"))?;

    match github::asset_ext() {
        "tar.gz" => {
            let tar_gz = flate2::read::GzDecoder::new(data);
            let mut archive = tar::Archive::new(tar_gz);
            archive
                .unpack(&tmpdir)
                .map_err(|e| format!("extract error: {e}"))?;
        }
        "zip" => {
            let cursor = std::io::Cursor::new(data);
            let mut archive =
                zip::ZipArchive::new(cursor).map_err(|e| format!("zip error: {e}"))?;
            archive
                .extract(&tmpdir)
                .map_err(|e| format!("extract error: {e}"))?;
        }
        other => return Err(format!("unsupported asset extension: {other}")),
    }

    let new_binary = tmpdir.join(github::binary_name());
    if !new_binary.exists() {
        return Err("binary not found in archive".to_string());
    }
    Ok(new_binary)
}

/// Print an advisory listing other running instances that should be restarted.
fn report_other_instances() {
    let others = swap::find_killable_pids();
    if !others.is_empty() {
        let list = others
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "note: {} other agentflare process(es) still running the old binary (pid {list}); \
             restart them to pick up this update",
            others.len()
        );
    }
}
