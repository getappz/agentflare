use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

const REPO: &str = "getappz/agentflare";
const API_LATEST: &str = "https://api.github.com/repos/getappz/agentflare/releases/latest";

fn target_triple() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "aarch64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        ""
    }
}

fn asset_ext() -> &'static str {
    if cfg!(windows) { "zip" } else { "tar.gz" }
}

fn asset_name(_version: &str) -> String {
    format!("agentflare-{}.{}", target_triple(), asset_ext())
}

fn checksums_name() -> String {
    "SHA256SUMS".to_string()
}

fn release_url(version: &str, asset: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{version}/{asset}")
}

fn gh_get(url: &str) -> Result<ureq::Response, String> {
    ureq::get(url)
        .set("User-Agent", "agentflare")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("HTTP error: {e}"))
}

pub fn latest_version() -> Result<String, String> {
    let resp = gh_get(API_LATEST)?;
    let json: serde_json::Value = resp.into_json().map_err(|e| format!("JSON parse: {e}"))?;
    json["tag_name"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "no tag_name in release".to_string())
}

pub fn check_for_update() -> Result<Option<String>, String> {
    let latest = latest_version()?;
    let current = format!("v{}", env!("CARGO_PKG_VERSION"));
    if latest != current {
        Ok(Some(latest))
    } else {
        Ok(None)
    }
}

pub fn run(version: Option<String>, check_only: bool, quiet: bool) {
    let target_version = match version {
        Some(ref v) => {
            let v = if !v.starts_with('v') {
                format!("v{v}")
            } else {
                v.clone()
            };
            if !quiet {
                println!("updating to {v}...");
            }
            v
        }
        None => {
            let latest = match check_for_update() {
                Ok(Some(v)) => v,
                Ok(None) => {
                    println!("agentflare is up to date (v{})", env!("CARGO_PKG_VERSION"));
                    return;
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
                return;
            }
            if !quiet {
                println!("updating to {latest}...");
            }
            latest
        }
    };

    let asset = asset_name(&target_version);
    let url = release_url(&target_version, &asset);

    if !quiet {
        println!("downloading {asset}...");
    }

    let resp = match gh_get(&url) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error downloading: {e}");
            std::process::exit(1);
        }
    };

    let mut data = Vec::new();
    resp.into_reader()
        .read_to_end(&mut data)
        .map_err(|e| format!("read error: {e}"))
        .unwrap();

    if !quiet {
        println!("verifying checksum...");
    }

    let expected = match expected_checksum(&target_version, &asset) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error fetching checksum: {e}");
            std::process::exit(1);
        }
    };

    let mut hasher = Sha256::new();
    hasher.update(&data);
    let actual = format!("{:x}", hasher.finalize());
    if actual != expected {
        eprintln!("checksum mismatch!\n  expected: {expected}\n  actual:   {actual}");
        std::process::exit(1);
    }

    let tmpdir = std::env::temp_dir().join(format!("agentflare-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmpdir);
    std::fs::create_dir_all(&tmpdir).unwrap();

    let binary_name = if cfg!(windows) {
        "agentflare.exe"
    } else {
        "agentflare"
    };

    match asset_ext() {
        "tar.gz" => {
            let tar_gz = flate2::read::GzDecoder::new(&data[..]);
            let mut archive = tar::Archive::new(tar_gz);
            archive
                .unpack(&tmpdir)
                .map_err(|e| format!("extract error: {e}"))
                .unwrap();
        }
        "zip" => {
            let cursor = std::io::Cursor::new(&data);
            let mut archive = zip::ZipArchive::new(cursor)
                .map_err(|e| format!("zip error: {e}"))
                .unwrap();
            archive
                .extract(&tmpdir)
                .map_err(|e| format!("extract error: {e}"))
                .unwrap();
        }
        _ => unreachable!(),
    }

    let new_binary = tmpdir.join(binary_name);
    if !new_binary.exists() {
        eprintln!("error: binary not found in archive");
        std::process::exit(1);
    }

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

    if let Err(e) = replace_binary(&new_binary, &current) {
        eprintln!("error replacing binary: {e}");
        std::process::exit(1);
    }

    if !quiet {
        println!("updated to {target_version}");
        println!("run `agentflare init --agent <agent>` to rewire hooks if needed");
    }
}

fn expected_checksum(version: &str, asset: &str) -> Result<String, String> {
    let url = release_url(version, &checksums_name());
    let resp = gh_get(&url)?;
    let text = resp.into_string().map_err(|e| format!("read error: {e}"))?;
    let needle = format!("  {asset}");
    for line in text.lines() {
        if line.ends_with(&needle) {
            return Ok(line.split_whitespace().next().unwrap_or("").to_string());
        }
    }
    Err(format!("checksum not found for {asset}"))
}

fn replace_binary(new_binary: &Path, current: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        let old = current.with_extension("old.exe");
        if old.exists() {
            std::fs::remove_file(&old).map_err(|e| format!("remove old: {e}"))?;
        }
        std::fs::rename(current, &old).map_err(|e| format!("rename: {e}"))?;
        std::fs::copy(new_binary, current).map_err(|e| format!("copy: {e}"))?;
        let _ = std::fs::remove_file(&old);
    }
    #[cfg(not(windows))]
    {
        let tmp = current.with_extension("new");
        std::fs::copy(new_binary, &tmp).map_err(|e| format!("copy: {e}"))?;
        std::fs::rename(&tmp, current).map_err(|e| format!("rename: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_name_uses_target_triple() {
        let name = asset_name("v1.0.0");
        assert!(name.contains("agentflare-"));
    }
}
