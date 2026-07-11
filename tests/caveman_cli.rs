//! Integration test for `agentflare caveman compress`, run against the
//! real compiled binary with a stubbed `claude` CLI on PATH — proves the
//! CLI wiring itself (arg parsing, RealLlm's CLI-path dispatch, exit code,
//! stdout format), not the LLM's actual compression quality (that's not
//! this binary's job to verify).

use std::path::PathBuf;
use std::process::Command;

/// Writes a fake `claude` executable that always prints a canned response
/// to stdout, regardless of stdin. Returns the directory it lives in (to
/// prepend to PATH) so callers can add it before the real `claude`, if any.
fn write_stub_claude(dir: &std::path::Path, response: &str) {
    #[cfg(windows)]
    {
        let path = dir.join("claude.cmd");
        let escaped = response.replace('%', "%%");
        std::fs::write(&path, format!("@echo off\r\necho {escaped}\r\n")).unwrap();
    }
    #[cfg(not(windows))]
    {
        let path = dir.join("claude");
        std::fs::write(&path, format!("#!/bin/sh\necho '{response}'\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }
}

fn agentflare_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_agentflare"))
}

#[test]
fn caveman_compress_generic_uses_the_stubbed_claude_cli() {
    let dir = tempfile::tempdir().unwrap();
    let stub_dir = dir.path().join("bin");
    std::fs::create_dir_all(&stub_dir).unwrap();
    write_stub_claude(&stub_dir, "compressed output");

    let source = dir.path().join("doc.md");
    std::fs::write(&source, "verbose original text").unwrap();

    let existing_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!(
        "{}{}{}",
        stub_dir.display(),
        if cfg!(windows) { ";" } else { ":" },
        existing_path
    );

    let output = Command::new(agentflare_bin())
        .args(["caveman", "compress"])
        .arg(&source)
        .env("PATH", new_path)
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("failed to run agentflare");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("B ▼"),
        "expected a summary line, got: {stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        "compressed output"
    );
    // Generic compression defaults to BackupMode::OutOfTree, so the backup
    // does NOT live next to the source as doc.md.orig — it lives under
    // dirs::cache_dir()/agentflare/caveman/backups/. Not asserted here
    // (that path isn't worth pinning down in this test); the source-content
    // assertion above already proves the compression itself took effect.
}
