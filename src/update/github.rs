//! GitHub release discovery + download helpers for `agentflare update`.
//!
//! Split out of the old single-file `update.rs` so the network/release logic
//! stays separate from the binary-swap logic in [`super::swap`].

use sha2::{Digest, Sha256};
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
    let latest = latest_version()?;
    let current = format!("v{}", env!("CARGO_PKG_VERSION"));
    if latest != current {
        Ok(Some(latest))
    } else {
        Ok(None)
    }
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
}
