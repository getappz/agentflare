// Scaffolding for the daemon HTTP server / foreground-daemon wiring,
// landing in a follow-up PR (see task-report.md). Not reachable yet.
#![allow(dead_code)]

use std::path::Path;

#[cfg(target_os = "macos")]
const CODESIGN_IDENTITY: &str = "agentflare-codesign";

#[cfg(target_os = "macos")]
pub fn setup_identity() -> Result<(), String> {
    if identity_exists() {
        return Ok(());
    }
    create_self_signed_identity()?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn identity_exists() -> bool {
    let output = std::process::Command::new("security")
        .args([
            "find-identity",
            "-v",
            "-p",
            "basic",
            "-s",
            CODESIGN_IDENTITY,
        ])
        .output();
    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

#[cfg(target_os = "macos")]
fn create_self_signed_identity() -> Result<(), String> {
    let keychain = "login.keychain";
    let status = std::process::Command::new("security")
        .args([
            "add-self-signed-cert",
            "-k",
            keychain,
            "-D",
            "Signing Identity",
            CODESIGN_IDENTITY,
        ])
        .status()
        .map_err(|e| format!("security add-self-signed-cert: {e}"))?;
    if !status.success() {
        let status2 = std::process::Command::new("security")
            .args([
                "add-generic-password",
                "-a",
                "agentflare",
                "-s",
                "agentflare-codesign-ref",
                "-w",
                CODESIGN_IDENTITY,
                "-U",
            ])
            .status()
            .map_err(|e| format!("security add-generic-password: {e}"))?;
        if !status2.success() {
            return Err("could not create codesign identity".to_string());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn sign_binary(path: &Path) -> Result<(), String> {
    let identity = if identity_exists() {
        CODESIGN_IDENTITY
    } else {
        "-"
    };
    let status = std::process::Command::new("codesign")
        .args([
            "--force",
            "--deep",
            "--sign",
            identity,
            "--options",
            "runtime",
            &path.to_string_lossy(),
        ])
        .status()
        .map_err(|e| format!("codesign: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("codesign exited with non-zero status".to_string())
    }
}

#[cfg(target_os = "macos")]
pub fn is_ready() -> bool {
    identity_exists()
}

#[cfg(not(target_os = "macos"))]
pub fn setup_identity() -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn sign_binary(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn is_ready() -> bool {
    true
}
