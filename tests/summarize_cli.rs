use std::process::Command;

#[test]
fn summarize_help_exposes_transcript_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .args(["summarize", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in ["--language", "--json", "--fetch-only"] {
        assert!(help.contains(flag), "missing {flag}");
    }
}

#[test]
fn summarize_rejects_invalid_url_without_network() {
    let output = Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .args(["summarize", "https://example.com/video", "--fetch-only"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("YouTube"));
}

#[test]
fn bundled_summarize_skill_is_served_without_network() {
    let output = Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .args(["skill", "get", "summarize"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("agentflare summarize"));
}
