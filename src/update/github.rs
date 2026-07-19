//! GitHub release discovery + download helpers for `agentflare update`.
//!
//! Split out of the old single-file `update.rs` so the network/release logic
//! stays separate from the binary-swap logic in [`super::swap`].

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::Duration;

pub(crate) const REPO: &str = "getappz/agentflare";

pub(crate) fn target_triple() -> &'static str {
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

pub(crate) fn asset_ext() -> &'static str {
    if cfg!(windows) { "zip" } else { "tar.gz" }
}

pub(crate) fn asset_name(_version: &str) -> String {
    format!("agentflare-{}.{}", target_triple(), asset_ext())
}

/// The binary filename inside a release archive for the current platform.
pub(crate) fn binary_name() -> &'static str {
    if cfg!(windows) {
        "agentflare.exe"
    } else {
        "agentflare"
    }
}

fn checksums_name() -> String {
    "SHA256SUMS".to_string()
}

fn release_url(version: &str, asset: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{version}/{asset}")
}

fn gh_get(url: &str) -> Result<ureq::Response, String> {
    // Bound connection and per-read stalls so a hung network never blocks the
    // update indefinitely. Deliberately no *overall* timeout: asset downloads
    // can be large and slow, and a per-read timeout already guards a stalled
    // socket without capping a legitimate long transfer.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(60))
        .build();
    agent
        .get(url)
        .set("User-Agent", "agentflare")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("HTTP error: {e}"))
}

fn cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("agentflare")
        .join("update-check")
}

/// Fresh-cache decision. `None` = cache too old (or missing), caller must hit
/// the network. `Some(None)` = fresh cache confirms `current` is up to date.
/// `Some(Some(v))` = fresh cache says version `v` is available. Split out
/// pure so the branching logic is unit-testable without touching disk.
fn cache_decision(cached_version: &str, current: &str, age_hours: i64) -> Option<Option<String>> {
    if age_hours >= 24 {
        return None;
    }
    if cached_version == current {
        Some(None)
    } else {
        Some(Some(cached_version.to_string()))
    }
}

fn read_cache() -> Result<Option<Option<String>>, String> {
    let path = cache_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let mut lines = content.lines();
    let timestamp = match lines.next() {
        Some(t) => t,
        None => return Ok(None),
    };
    let cached_version = match lines.next() {
        Some(v) => v,
        None => return Ok(None),
    };
    let cached_time = match chrono::DateTime::parse_from_rfc3339(timestamp) {
        Ok(t) => t,
        Err(_) => return Ok(None),
    };
    let age_hours = chrono::Utc::now()
        .signed_duration_since(cached_time)
        .num_hours();
    let current = format!("v{}", env!("CARGO_PKG_VERSION"));
    Ok(cache_decision(cached_version, &current, age_hours))
}

fn write_cache(version: &str) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = format!("{}\n{}\n", chrono::Utc::now().to_rfc3339(), version);
    let _ = std::fs::write(&path, content);
}

pub(crate) fn latest_version() -> Result<String, String> {
    let client = crate::github::Client::anonymous();
    let json = client
        .request("GET", "/repos/getappz/agentflare/releases/latest", None)
        .map_err(|e| e.to_string())?;
    json["tag_name"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "no tag_name in release".to_string())
}

pub(crate) fn check_for_update() -> Result<Option<String>, String> {
    // Check the 24h cache first.
    if let Some(cached) = read_cache()? {
        return Ok(cached);
    }

    let latest = match latest_version() {
        Ok(v) => v,
        Err(e) => {
            // Network error: fall back to stale cache, if any.
            if let Ok(Some(cached)) = read_stale_cache() {
                return Ok(Some(cached));
            }
            return Err(e);
        }
    };

    let current = format!("v{}", env!("CARGO_PKG_VERSION"));

    // Cache the result regardless of whether there's an update.
    write_cache(&latest);

    if latest != current {
        Ok(Some(latest))
    } else {
        Ok(None)
    }
}

/// Read the cache file even when it's stale — used as a network-failure
/// fallback.
fn read_stale_cache() -> Result<Option<String>, String> {
    let path = cache_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    let version = content.lines().nth(1).unwrap_or("");
    if version.is_empty() {
        return Ok(None);
    }
    Ok(Some(version.to_string()))
}

/// Download the release asset for `version` and return its bytes.
pub(crate) fn download_asset(version: &str, asset: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let url = release_url(version, asset);
    let resp = gh_get(&url)?;
    let mut data = Vec::new();
    resp.into_reader()
        .read_to_end(&mut data)
        .map_err(|e| format!("read error: {e}"))?;
    Ok(data)
}

/// Fetch `SHA256SUMS` for `version` and return the checksum recorded for `asset`.
pub(crate) fn expected_checksum(version: &str, asset: &str) -> Result<String, String> {
    let url = release_url(version, &checksums_name());
    let resp = gh_get(&url)?;
    let text = resp.into_string().map_err(|e| format!("read error: {e}"))?;
    parse_checksum(&text, asset).ok_or_else(|| format!("checksum not found for {asset}"))
}

/// Pull the checksum for `asset` out of a `SHA256SUMS` body. Pure so it can be
/// unit-tested without the network.
fn parse_checksum(sums: &str, asset: &str) -> Option<String> {
    let needle = format!("  {asset}");
    for line in sums.lines() {
        if line.ends_with(&needle) {
            return line.split_whitespace().next().map(|s| s.to_string());
        }
    }
    None
}

/// Verify `data` hashes to `expected` (lowercase hex SHA-256).
pub(crate) fn verify_checksum(data: &[u8], expected: &str) -> Result<(), String> {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let actual = format!("{:x}", hasher.finalize());
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch!\n  expected: {expected}\n  actual:   {actual}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_name_uses_target_triple() {
        let name = asset_name("v1.0.0");
        assert!(name.contains("agentflare-"));
    }

    #[test]
    fn parse_checksum_finds_the_matching_asset() {
        let sums = "aaaa  agentflare-x86_64-apple-darwin.tar.gz\n\
                    bbbb  agentflare-x86_64-pc-windows-msvc.zip\n";
        assert_eq!(
            parse_checksum(sums, "agentflare-x86_64-pc-windows-msvc.zip").as_deref(),
            Some("bbbb")
        );
        assert_eq!(parse_checksum(sums, "no-such-asset").as_deref(), None);
    }

    #[test]
    fn parse_checksum_ignores_prefix_substring_collisions() {
        // A different asset whose name ends with the same suffix must not match:
        // matching requires the two-space + full-name boundary.
        let sums = "cccc  other-agentflare-x86_64-apple-darwin.tar.gz\n";
        assert_eq!(
            parse_checksum(sums, "agentflare-x86_64-apple-darwin.tar.gz").as_deref(),
            None
        );
    }

    #[test]
    fn verify_checksum_accepts_correct_and_rejects_wrong() {
        // sha256("") = e3b0c442...
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(verify_checksum(b"", empty).is_ok());
        assert!(verify_checksum(b"tampered", empty).is_err());
    }

    #[test]
    fn cache_decision_caches_up_to_date_result() {
        // Regression: previously the "you're already on latest" case was
        // indistinguishable from "no cache", so it was never cached and hit
        // the network on every invocation.
        assert_eq!(cache_decision("v1.0.0", "v1.0.0", 1), Some(None));
    }

    #[test]
    fn cache_decision_returns_cached_update() {
        assert_eq!(
            cache_decision("v1.2.0", "v1.0.0", 1),
            Some(Some("v1.2.0".to_string()))
        );
    }

    #[test]
    fn cache_decision_expires_after_24h() {
        assert_eq!(cache_decision("v1.0.0", "v1.0.0", 24), None);
    }
}
