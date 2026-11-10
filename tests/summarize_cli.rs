use std::process::Command;

#[test]
fn summarize_help_exposes_transcript_options() {
    let output = Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .args(["summarize", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    for flag in [
        "--language",
        "--json",
        "--fetch-only",
        "--extract",
        "--format",
        "--length",
        "--summary-language",
        "--model",
        "--prompt",
        "--prompt-file",
        "--timestamps",
    ] {
        assert!(help.contains(flag), "missing {flag}");
    }
}

#[test]
fn extract_rejects_summary_options_before_retrieval() {
    let output = Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .args([
            "summarize",
            "https://youtu.be/snErQUyqwCU",
            "--extract",
            "--length",
            "short",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--length"));
}

#[test]
fn unsupported_formats_and_conflicting_prompts_fail_before_retrieval() {
    for args in [
        vec![
            "summarize",
            "https://youtu.be/snErQUyqwCU",
            "--format",
            "yaml",
        ],
        vec![
            "summarize",
            "https://youtu.be/snErQUyqwCU",
            "--prompt",
            "Focus",
            "--prompt-file",
            "missing.txt",
        ],
        vec![
            "summarize",
            "https://youtu.be/snErQUyqwCU",
            "--json",
            "--format",
            "md",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_agentflare"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}

#[test]
fn summarize_rejects_invalid_url_without_network() {
    for mode in [
        vec![],
        vec!["--extract", "--json"],
        vec!["--extract", "--format", "md", "--timestamps"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_agentflare"))
            .args(["summarize", "https://example.com/video", "--fetch-only"])
            .args(mode)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("YouTube"));
    }
}

#[test]
fn missing_prompt_file_fails_before_fetching_a_video() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .args(["summarize", "https://youtu.be/snErQUyqwCU", "--prompt-file"])
        .arg(dir.path().join("missing.txt"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read prompt file"));
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
