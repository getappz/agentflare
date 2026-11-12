const TERMINAL_EVENT: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"verified 🦀","session_id":"fixture-session","total_cost_usd":0.125,"usage":{"input_tokens":30,"output_tokens":7}}"#;

#[test]
#[ignore = "subprocess fixture for Claude result lifecycle"]
fn terminal_claude_fixture() {
    use std::io::Write;
    let mode = std::env::var("AGENTFLARE_TEST_RESULT_MODE").unwrap();
    let event = match mode.as_str() {
        "error" => {
            r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"failed"}"#
        }
        "cap" => r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#,
        "partial" => &TERMINAL_EVENT[..TERMINAL_EVENT.len() / 2],
        _ => TERMINAL_EVENT,
    };
    if mode == "stderr" {
        writeln!(std::io::stderr(), "{event}").unwrap();
    } else {
        let mut out = std::io::stdout();
        // Deliberately split inside a multi-byte code point.
        let split = event.find('🦀').map_or(event.len() / 2, |at| at + 1);
        out.write_all(&event.as_bytes()[..split]).unwrap();
        out.flush().unwrap();
        std::thread::sleep(Duration::from_millis(40));
        out.write_all(&event.as_bytes()[split..]).unwrap();
        out.flush().unwrap();
        if mode != "unterminated" {
            writeln!(out, "\nbackground cleanup log").unwrap();
        }
    }
    if mode == "nonzero" {
        std::process::exit(17);
    }
    std::thread::sleep(Duration::from_secs(30));
}

fn terminal_fixture_command(mode: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "--ignored",
        "--exact",
        "agent_launch::tests::terminal_claude_fixture",
        "--nocapture",
        "--test-threads=2",
    ])
    .env("AGENTFLARE_TEST_RESULT_MODE", mode)
    .env("RUST_TEST_THREADS", "1");
    cmd
}

/// Claude Code's stream: parsed, and its terminal result ends the run.
const ENDS_AT_RESULT: Option<agent_registry::Structured> = Some(agent_registry::Structured {
    args: &[],
    format: agent_registry::ReplyFormat::ResultObject,
    ends_at_result: true,
});
/// The same stream for an agent whose terminal event is not verified.
const PARSE_ONLY: Option<agent_registry::Structured> = Some(agent_registry::Structured {
    args: &[],
    format: agent_registry::ReplyFormat::ResultObject,
    ends_at_result: false,
});

fn terminal_capture(
    mode: &str,
    stream: Option<agent_registry::Structured>,
    cap: Duration,
    cancel: Option<&str>,
) -> Captured {
    run_captured_with_completion(
        terminal_fixture_command(mode),
        cap,
        Duration::from_millis(500),
        None,
        cancel,
        StallConfig::from_env(),
        stream.map(|s| (Agent::ClaudeCode, Stream::Cli(s))),
    )
    .unwrap()
}

fn saw_terminal(c: &Captured) -> bool {
    c.turn.as_ref().is_some_and(|t| t.terminal)
}

#[test]
fn terminal_claude_capture_preserves_utf8_and_metadata_with_or_without_newline() {
    for mode in ["success", "unterminated"] {
        let c = terminal_capture(mode, ENDS_AT_RESULT, Duration::from_secs(10), None);
        assert!(c.success && !c.timed_out);
        let reply = c.turn.unwrap().into_reply(&c.stdout);
        assert_eq!(reply.text, "verified 🦀");
        assert_eq!(reply.cost_usd, Some(0.125));
        assert_eq!(reply.input_tokens, Some(30));
        assert_eq!(reply.output_tokens, Some(7));
        assert_eq!(reply.session_id.as_deref(), Some("fixture-session"));
    }
}

#[test]
fn terminal_claude_result_never_overrides_failure_or_cancellation() {
    let c = terminal_capture("nonzero", ENDS_AT_RESULT, Duration::from_secs(10), None);
    assert!(!c.success && !c.timed_out && saw_terminal(&c));
    let c = terminal_capture("success", ENDS_AT_RESULT, Duration::from_millis(300), None);
    assert!(!c.success && c.timed_out && !c.idle_killed);
    let _guard = agentflare_jobs::cancel::register("terminal-result-cancel", || true);
    let c = terminal_capture(
        "success",
        ENDS_AT_RESULT,
        Duration::from_secs(10),
        Some("terminal-result-cancel"),
    );
    assert!(!c.success && c.cancelled);
}

#[test]
fn terminal_completion_is_gated_per_agent_and_ignores_stderr_and_partial_json() {
    for (mode, stream) in [
        ("success", None),
        ("success", PARSE_ONLY),
        ("stderr", ENDS_AT_RESULT),
        ("partial", ENDS_AT_RESULT),
    ] {
        let c = terminal_capture(mode, stream, Duration::from_secs(10), None);
        assert!(!c.success && c.idle_killed, "{mode}");
        // Parse-only still sees the result; it just may not end the run.
        assert_eq!(saw_terminal(&c), stream == PARSE_ONLY, "{mode}");
    }
}

#[test]
fn terminal_claude_errors_keep_existing_classification() {
    for (mode, succeeds) in [("error", false), ("cap", true)] {
        let c = terminal_capture(mode, ENDS_AT_RESULT, Duration::from_secs(10), None);
        assert!(c.success);
        let reply = c.turn.unwrap().into_reply(&c.stdout);
        assert_eq!(
            matches!(
                classify_result_reply("fixture", reply),
                HeadlessOutcome::Ok(_)
            ),
            succeeds
        );
    }
}

fn lingering_claude_registry(root: &Path, event: &str) -> Vec<AgentSpec> {
    #[cfg(windows)]
    let (name, script) = (
        "fake-claude.ps1",
        format!(
            "param([switch]$p)\n[Console]::Out.WriteLine('{}')\nStart-Sleep -Seconds 30\n",
            event.replace('\'', "''")
        ),
    );
    #[cfg(not(windows))]
    let (name, script) = (
        "fake-claude",
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}'\nsleep 30\n",
            event.replace('\'', "'\\''")
        ),
    );
    let path = root.join(name);
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let binary: &'static str = Box::leak(path.to_string_lossy().into_owned().into_boxed_str());
    vec![AgentSpec {
        id: Agent::ClaudeCode,
        display_name: "fixture Claude",
        tier: Tier::Cli,
        binary_names: Box::leak(vec![binary].into_boxed_slice()),
        version_args: &[],
        package_manager: None,
        package_name: None,
    }]
}

#[test]
fn terminal_claude_result_survives_lingering_process() {
    let root = tempfile::tempdir().unwrap();
    let event = r#"{"type":"result","subtype":"success","is_error":false,"result":"verified","session_id":"fixture-session","total_cost_usd":0.125,"usage":{"input_tokens":30,"output_tokens":7}}"#;
    let registry = lingering_claude_registry(root.path(), event);
    let outcome = run_headless_in_with_owner(
        None,
        root.path(),
        &registry,
        "claude-code",
        "task",
        Duration::from_secs(15),
        Duration::from_secs(5),
        &[],
        true,
    );
    match outcome {
        HeadlessOutcome::Ok(reply) => {
            assert_eq!(reply.text, "verified");
            assert_eq!(reply.cost_usd, Some(0.125));
            assert_eq!(reply.session_id.as_deref(), Some("fixture-session"));
            assert_eq!(reply.input_tokens, Some(30));
            assert_eq!(reply.output_tokens, Some(7));
        }
        other => panic!("a complete result must not be retried as an idle failure: {other:?}"),
    }
}
