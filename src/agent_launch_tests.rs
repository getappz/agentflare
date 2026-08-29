    use super::*;
    #[cfg(unix)]
    use agent_registry::detect::PATH_LOCK as GLOBAL_STATE_LOCK;
    use agent_registry::{Agent, Tier};

    fn test_registry() -> Vec<AgentSpec> {
        vec![
            AgentSpec {
                id: Agent::Aider,
                display_name: "aider",
                tier: Tier::Cli,
                binary_names: &["aider"],
                version_args: &[],
                package_manager: None,
                package_name: None,
            },
            AgentSpec {
                id: Agent::Cline,
                display_name: "cline",
                tier: Tier::Extension,
                binary_names: &[],
                version_args: &[],
                package_manager: None,
                package_name: None,
            },
        ]
    }

    #[test]
    fn launch_unknown_agent_errors() {
        let reg = test_registry();
        match run_launch(&reg, "nonexistent", None, None, &[]) {
            LaunchOutcome::UnknownAgent(msg) => assert!(msg.contains("nonexistent")),
            _ => panic!("expected UnknownAgent"),
        }
    }

    #[test]
    fn launch_extension_agent_errors() {
        let reg = test_registry();
        match run_launch(&reg, "cline", None, None, &[]) {
            LaunchOutcome::Extension(msg) => assert!(msg.contains("editor extension")),
            _ => panic!("expected Extension"),
        }
    }

    #[test]
    fn launch_not_on_path_errors() {
        let reg = test_registry();
        // "aider" unlikely to be on a test PATH
        match run_launch(&reg, "aider", None, None, &[]) {
            LaunchOutcome::NotFound(msg) => assert!(msg.contains("not found on PATH")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // Process spawning is exercised via a POSIX shell; gate to Unix so the
    // Windows CI (no `sh`/`sleep`) never runs it.
    #[cfg(unix)]
    #[test]
    fn run_captured_captures_stdout() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'hello world'");
        let out = run_captured(
            cmd,
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(5),
            None,
        )
        .unwrap();
        assert!(out.success);
        assert!(!out.timed_out);
        assert_eq!(out.stdout, "hello world");
    }

    #[cfg(unix)]
    #[test]
    fn run_captured_keeps_output_written_before_a_timeout_kill() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'made it here'; sleep 5");
        let out = run_captured(
            cmd,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_millis(150),
            None,
        )
        .unwrap();
        assert!(out.timed_out);
        assert_eq!(
            out.stdout, "made it here",
            "output written before the kill must still be captured, not discarded"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_captured_times_out_and_kills_the_child() {
        let start = std::time::Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 5");
        let out = run_captured(
            cmd,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_millis(150),
            None,
        )
        .unwrap();
        assert!(out.timed_out, "should report timeout");
        assert!(!out.success, "a killed child is not a success");
        // Must return promptly, not wait out the full 5s sleep.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "did not kill promptly"
        );
    }

    // A process that keeps producing output must NOT be killed just because
    // it has run past what used to be the single fixed timeout — only a
    // genuinely stalled child (no new bytes for `idle_timeout`) should be.
    // This is the behavior item #20 exists to add.
    #[cfg(unix)]
    #[test]
    fn run_captured_does_not_idle_timeout_while_output_keeps_arriving() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("for i in 1 2 3 4 5; do printf 'x'; sleep 0.05; done");
        let out = run_captured(
            cmd,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(300),
            None,
        )
        .unwrap();
        assert!(
            !out.timed_out,
            "steady output growth must reset the idle clock, not get killed"
        );
        assert_eq!(out.stdout, "xxxxx");
    }

    // Mirrors `run_captured_keeps_output_written_before_a_timeout_kill` but
    // with a generous hard cap and a tight idle window, proving the idle
    // timeout — not the hard cap — is what catches a child that produced
    // real output and then went quiet.
    #[cfg(unix)]
    #[test]
    fn run_captured_idle_timeout_kills_a_stalled_child_well_before_the_hard_cap() {
        let start = std::time::Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("printf 'did some work'; sleep 5");
        let out = run_captured(
            cmd,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_millis(150),
            None,
        )
        .unwrap();
        assert!(out.timed_out, "should time out");
        assert!(
            out.idle_killed,
            "should be killed for going idle, not for exceeding the hard cap"
        );
        assert_eq!(out.stdout, "did some work");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "idle timeout should fire well before the 30s hard cap"
        );
    }

    // Proves the fix for the "descendant outlives the direct child" hang: the
    // direct child backgrounds a grandchild that inherits the piped stdout fd,
    // then waits on it. If timeout only killed the direct child (the old
    // `child.kill()` behavior), the grandchild would keep the pipe's write end
    // open and `reader.join()` would block for the full 5s sleep. With
    // process-group tree-killing, both die together and this returns promptly.
    #[cfg(unix)]
    #[test]
    fn run_captured_times_out_and_kills_the_whole_tree() {
        let start = std::time::Instant::now();
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("sleep 5 & wait");
        let out = run_captured(
            cmd,
            std::time::Duration::from_millis(150),
            std::time::Duration::from_millis(150),
            None,
        )
        .unwrap();
        assert!(out.timed_out, "should report timeout");
        assert!(!out.success, "a killed child is not a success");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "did not kill the whole tree promptly — a descendant likely kept the stdout pipe open"
        );
    }

    // Item #139: an ambient CARGO_TARGET_DIR must never reach the launched
    // agent — Cargo's env var always outranks the worktree's isolated
    // `.cargo/config.toml` (see `isolate_worktree_target_dir` in
    // worktree.rs), so leaking it here would silently defeat that isolation
    // for every build the agent runs. `env_remove` clones the current env
    // and drops the key at that point, so this holds regardless of what any
    // other test concurrently does to the ambient var.
    #[cfg(unix)]
    #[test]
    fn run_launch_env_strips_ambient_cargo_target_dir() {
        // SAFETY: GLOBAL_STATE_LOCK serializes this process-wide env
        // mutation against every other test touching CARGO_TARGET_DIR/PATH.
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let marker = tempfile::NamedTempFile::new().unwrap();
        let marker_path = marker.path().to_path_buf();
        unsafe {
            std::env::set_var("CARGO_TARGET_DIR", "/tmp/shared-target");
        }
        let reg = vec![AgentSpec {
            id: Agent::Aider,
            display_name: "aider",
            tier: Tier::Cli,
            binary_names: &["sh"],
            version_args: &[],
            package_manager: None,
            package_name: None,
        }];
        // `printf`, not `echo -n`: on macOS's /bin/sh, `-n` isn't a flag
        // (that's a bash builtin behavior), so `echo -n` would literally
        // print "-n" into the marker instead of suppressing the newline.
        let script = format!(
            "printf '%s' \"$CARGO_TARGET_DIR\" > {}",
            marker_path.display()
        );
        run_launch_env(
            &reg,
            "aider",
            None,
            None,
            &["-c".to_string(), script],
            &[],
            false,
        );
        unsafe {
            std::env::remove_var("CARGO_TARGET_DIR");
        }
        let content = std::fs::read_to_string(&marker_path).unwrap();
        assert_eq!(
            content, "",
            "child must not inherit ambient CARGO_TARGET_DIR"
        );
    }

    // An explicit CARGO_TARGET_DIR passed in `env` (e.g. sourced from a
    // project's `.dev.vars`) must not reintroduce the var `env_remove`
    // above just stripped, or a dev-vars file that happens to set it would
    // silently defeat the ambient-isolation guarantee this launch path
    // exists for.
    #[cfg(unix)]
    #[test]
    fn run_launch_env_override_cannot_reintroduce_cargo_target_dir() {
        // SAFETY: GLOBAL_STATE_LOCK serializes against other tests that
        // mutate CARGO_TARGET_DIR in the ambient process env.
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let marker = tempfile::NamedTempFile::new().unwrap();
        let marker_path = marker.path().to_path_buf();
        let reg = vec![AgentSpec {
            id: Agent::Aider,
            display_name: "aider",
            tier: Tier::Cli,
            binary_names: &["sh"],
            version_args: &[],
            package_manager: None,
            package_name: None,
        }];
        let script = format!(
            "printf '%s' \"$CARGO_TARGET_DIR\" > {}",
            marker_path.display()
        );
        run_launch_env(
            &reg,
            "aider",
            None,
            None,
            &["-c".to_string(), script],
            &[(
                "CARGO_TARGET_DIR".to_string(),
                "/should/not/leak".to_string(),
            )],
            false,
        );
        let content = std::fs::read_to_string(&marker_path).unwrap();
        assert_eq!(
            content, "",
            "explicit env override must not reintroduce CARGO_TARGET_DIR"
        );
    }

    fn headless_registry() -> Vec<AgentSpec> {
        vec![
            AgentSpec {
                id: Agent::Codex,
                display_name: "codex",
                tier: Tier::Cli,
                // A binary name that will never resolve on PATH.
                binary_names: &["definitely-not-a-real-binary-xyz"],
                version_args: &[],
                package_manager: None,
                package_name: None,
            },
            AgentSpec {
                id: Agent::Aider, // Cli, but no headless print mode mapped
                display_name: "aider",
                tier: Tier::Cli,
                binary_names: &["aider"],
                version_args: &[],
                package_manager: None,
                package_name: None,
            },
        ]
    }

    #[test]
    fn headless_argv_carries_no_prompt_element() {
        // The prompt is piped over stdin by `run_headless`, not appended to
        // argv (item #75) — argv is just the binary plus print-mode flags.
        let binary = std::path::Path::new("/usr/bin/claude");
        assert_eq!(
            headless_argv(Agent::ClaudeCode, binary, &[]),
            Some(vec!["/usr/bin/claude".to_string(), "-p".to_string()])
        );
        assert_eq!(
            headless_argv(Agent::Codex, std::path::Path::new("/x/codex"), &[]),
            Some(vec!["/x/codex".to_string(), "exec".to_string()])
        );
    }

    #[test]
    fn headless_argv_none_without_print_mode() {
        assert_eq!(
            headless_argv(Agent::Aider, std::path::Path::new("/x/aider"), &[]),
            None
        );
    }

    #[test]
    fn run_headless_unknown_agent() {
        let reg = headless_registry();
        match run_headless(
            &reg,
            "nope",
            "hi",
            Duration::from_secs(1),
            Duration::from_secs(1),
            &[],
            false,
        ) {
            HeadlessOutcome::UnknownAgent(m) => assert!(m.contains("nope")),
            other => panic!("expected UnknownAgent, got {other:?}"),
        }
    }

    #[test]
    fn run_headless_agent_without_print_mode() {
        let reg = headless_registry();
        match run_headless(
            &reg,
            "aider",
            "hi",
            Duration::from_secs(1),
            Duration::from_secs(1),
            &[],
            false,
        ) {
            HeadlessOutcome::NotHeadless(m) => assert!(m.contains("aider")),
            other => panic!("expected NotHeadless, got {other:?}"),
        }
    }

    #[test]
    #[ignore]
    fn repro_cursor_cmd_hang() {
        let t0 = std::time::Instant::now();
        let out = super::run_headless(
            agent_registry::REGISTRY,
            "cursor",
            "Reply with exactly: ok",
            Duration::from_secs(120),
            Duration::from_secs(60),
            &["--force".to_string()],
            false,
        );
        eprintln!("elapsed={:?}", t0.elapsed());
        match out {
            super::HeadlessOutcome::Ok(reply) => eprintln!("OK reply={:?}", reply),
            other => eprintln!("NOT OK: {other:?}"),
        }
    }

    #[test]
    fn run_headless_binary_not_found() {
        let reg = headless_registry();
        match run_headless(
            &reg,
            "codex",
            "hi",
            Duration::from_secs(1),
            Duration::from_secs(1),
            &[],
            false,
        ) {
            HeadlessOutcome::NotFound(m) => assert!(m.contains("not found")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    // Item #75: a prompt built from an item's full comment history can
    // exceed Linux's MAX_ARG_STRLEN (128 KiB on common configurations), the
    // max length of any single argv element — separate from and much
    // smaller than total ARG_MAX. Passing the prompt as a trailing argv
    // element (the old behavior) made `Command::spawn()` fail with E2BIG
    // once a prompt crossed that ceiling. `sh -p -c 'wc -c'` stands in for a
    // headless agent binary: `-p` is `headless_args(ClaudeCode)`'s flag
    // (harmless to `sh`, which treats it as the POSIX "privileged" option),
    // and `wc -c` reports exactly how many bytes it received on stdin —
    // proving the whole oversized prompt arrived intact via stdin rather
    // than argv.
    #[cfg(unix)]
    #[test]
    fn run_headless_pipes_a_prompt_over_the_argv_length_limit_via_stdin() {
        let reg = vec![AgentSpec {
            id: Agent::ClaudeCode,
            display_name: "claude-code",
            tier: Tier::Cli,
            binary_names: &["sh"],
            version_args: &[],
            package_manager: None,
            package_name: None,
        }];
        // Linux's MAX_ARG_STRLEN is 128 KiB (32 * 4 KiB pages); comfortably
        // exceed it so this would have hit E2BIG under the old argv-based
        // prompt delivery.
        let huge_prompt = "x".repeat(200 * 1024);
        match run_headless(
            &reg,
            "claude-code",
            &huge_prompt,
            Duration::from_secs(10),
            Duration::from_secs(10),
            &["-c".to_string(), "wc -c".to_string()],
            false,
        ) {
            HeadlessOutcome::Ok(reply) => {
                assert_eq!(
                    reply.text.trim(),
                    huge_prompt.len().to_string(),
                    "the full oversized prompt should have arrived via stdin"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    // `run_headless_in` differs from `run_headless` in exactly one way: it
    // sets the child's cwd explicitly via `Command::current_dir` instead of
    // relying on the caller's ambient cwd (see the doc comment on
    // `run_headless_impl`). `sh -c pwd` reports back whatever cwd the child
    // actually launched in, proving the explicit `cwd` argument reached the
    // spawned process rather than being ignored.
    #[cfg(unix)]
    #[test]
    fn run_headless_in_launches_with_an_explicit_cwd() {
        let reg = vec![AgentSpec {
            id: Agent::ClaudeCode,
            display_name: "claude-code",
            tier: Tier::Cli,
            binary_names: &["sh"],
            version_args: &[],
            package_manager: None,
            package_name: None,
        }];
        let dir = tempfile::tempdir().unwrap();
        let expected = std::fs::canonicalize(dir.path()).unwrap();

        match run_headless_in(
            dir.path(),
            &reg,
            "claude-code",
            "ignored prompt",
            Duration::from_secs(10),
            Duration::from_secs(10),
            &["-c".to_string(), "pwd".to_string()],
            false,
        ) {
            HeadlessOutcome::Ok(reply) => {
                assert_eq!(
                    std::path::Path::new(reply.text.trim()),
                    expected.as_path(),
                    "child should have launched with the explicit cwd"
                );
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn parse_claude_reply_extracts_structured_fields() {
        let raw = r#"{"result":"Fixed the race by adding a mutex.","session_id":"sess-123","total_cost_usd":0.0842}"#;
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, "Fixed the race by adding a mutex.");
        assert_eq!(session_id.as_deref(), Some("sess-123"));
        assert_eq!(cost, Some(0.0842));
    }

    #[test]
    fn parse_claude_reply_extracts_the_result_from_the_last_line_of_a_stream_json_transcript() {
        // --output-format stream-json emits one JSON object per line (system
        // init, tool_use/tool_result, assistant messages, ...) and only the
        // FINAL line carries the same {"result":...} shape the single-object
        // `json` format uses — everything before it must be ignored, not
        // treated as (or blended into) the reply text. This is the exact
        // shape a judge's raw reply takes (item #489): the first line is a
        // valid-but-`action`-less JSON object, which `parse_judge_decision`
        // used to grab if this function wasn't called first.
        let raw = concat!(
            r#"{"type":"system","subtype":"init","session_id":"sess-123"}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working..."}]}}"#,
            "\n",
            r#"{"type":"result","result":"Fixed the race by adding a mutex.","session_id":"sess-123","total_cost_usd":0.0842}"#,
        );
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, "Fixed the race by adding a mutex.");
        assert_eq!(session_id.as_deref(), Some("sess-123"));
        assert_eq!(cost, Some(0.0842));
    }

    #[test]
    fn clean_agent_reply_is_a_no_op_for_non_claude_agents() {
        let raw = "DONE: added the flag".to_string();
        assert_eq!(
            clean_agent_reply(Agent::Opencode.as_str(), raw.clone()),
            raw
        );
    }

    #[test]
    fn clean_agent_reply_parses_cursor_stream_json() {
        let raw = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working..."}]}}"#,
            "\n",
            r#"{"type":"result","result":"single line content again","session_id":"sess-456"}"#,
        );
        let (text, session_id, _) = parse_claude_reply(raw);
        assert_eq!(text, "single line content again");
        assert_eq!(session_id.as_deref(), Some("sess-456"));
        assert_eq!(
            clean_agent_reply(Agent::Cursor.as_str(), raw.to_string()),
            "single line content again"
        );
    }

    #[test]
    fn parse_claude_reply_falls_back_to_raw_text_on_non_json() {
        let raw = "plain text reply, no JSON here";
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, raw);
        assert!(session_id.is_none());
        assert!(cost.is_none());
    }

    #[test]
    fn tail_str_returns_the_whole_string_when_shorter_than_the_limit() {
        assert_eq!(tail_str("hello", 100), "hello");
    }

    #[test]
    fn tail_str_returns_only_the_last_n_chars() {
        assert_eq!(tail_str("abcdefgh", 3), "fgh");
    }

    #[test]
    fn tail_str_does_not_panic_on_a_multibyte_boundary() {
        // Each of these is a 3-4 byte UTF-8 char; slicing by raw byte offset
        // instead of char boundary would panic here.
        let s = "a€b€c€d€e€f€g€h€i€j";
        // Must not panic, and must return valid, non-empty UTF-8.
        let tail = tail_str(s, 5);
        assert!(!tail.is_empty());
        assert!(tail.chars().count() <= 5);
    }

    #[test]
    fn diagnostic_suffix_reports_no_output_captured_when_both_streams_are_empty() {
        let c = Captured {
            success: false,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            idle_killed: false,
        };
        assert_eq!(diagnostic_suffix(&c, None), " (no output captured)");
    }

    #[test]
    fn diagnostic_suffix_prefers_stdout_over_stderr() {
        let c = Captured {
            success: false,
            stdout: "working on task 3...".to_string(),
            stderr: "some warning".to_string(),
            timed_out: true,
            idle_killed: false,
        };
        let suffix = diagnostic_suffix(&c, None);
        assert!(suffix.contains("last stdout before kill"));
        assert!(suffix.contains("working on task 3..."));
    }

    #[test]
    fn diagnostic_suffix_falls_back_to_stderr_when_stdout_is_empty() {
        let c = Captured {
            success: false,
            stdout: String::new(),
            stderr: "panic: something broke".to_string(),
            timed_out: true,
            idle_killed: false,
        };
        let suffix = diagnostic_suffix(&c, None);
        assert!(suffix.contains("last stderr before kill"));
        assert!(suffix.contains("panic: something broke"));
    }

    #[test]
    fn diagnostic_suffix_appends_sandbox_log_even_when_stdout_is_empty() {
        // Item #139: the sandbox-side log (e.g. opencode's own tool-call
        // trace) is a different signal from stdout/stderr and must survive
        // even when the agent produced no reply text at all before being
        // killed -- the exact "black box" case this feature exists for.
        let c = Captured {
            success: false,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            idle_killed: true,
        };
        let suffix = diagnostic_suffix(&c, Some("level=INFO message=\"tool call\" tool=lean_ctx"));
        assert!(suffix.contains("sandbox-side agent log tail"));
        assert!(suffix.contains("tool=lean_ctx"));
    }

    #[test]
    fn failed_non_zero_exit_includes_captured_output() {
        let captured = Captured {
            stdout: String::new(),
            stderr: "HTTP 429 Too Many Requests".to_string(),
            success: false,
            timed_out: false,
            idle_killed: false,
        };
        let msg = match Ok::<_, std::io::Error>(captured) {
            Ok(c) if c.success => unreachable!(),
            Ok(c) if c.timed_out => unreachable!(),
            Ok(c) => format!("test exited non-zero{}", diagnostic_suffix(&c, None)),
            Err(_) => unreachable!(),
        };
        assert!(
            msg.contains("HTTP 429 Too Many Requests"),
            "expected captured stderr in the failure message, got: {msg}"
        );
    }

    #[test]
    fn take_diagnostic_log_reads_and_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("diag.log");
        std::fs::write(&path, "some diagnostic content\n").unwrap();
        let content = take_diagnostic_log(Some(&path));
        assert_eq!(content.as_deref(), Some("some diagnostic content\n"));
        assert!(!path.exists(), "diagnostic file should be removed after reading");
    }

    #[test]
    fn take_diagnostic_log_returns_none_for_missing_or_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.log");
        assert_eq!(take_diagnostic_log(Some(&missing)), None);

        let empty = dir.path().join("empty.log");
        std::fs::write(&empty, "   \n").unwrap();
        assert_eq!(take_diagnostic_log(Some(&empty)), None);
        assert!(!empty.exists());

        assert_eq!(take_diagnostic_log(None), None);
    }

    #[test]
    fn parse_json_reply_extracts_result_session_and_cost() {
        let reply = parse_json_reply(
            r#"{"type":"result","result":"pong","session_id":"abc-123","total_cost_usd":0.05}"#,
        );
        assert_eq!(reply.text, "pong");
        assert_eq!(reply.session_id.as_deref(), Some("abc-123"));
        assert_eq!(reply.cost_usd, Some(0.05));
    }

    #[test]
    fn parse_json_reply_handles_missing_cost_usd() {
        let reply = parse_json_reply(r#"{"type":"result","result":"pong","session_id":"abc-123"}"#);
        assert_eq!(reply.text, "pong");
        assert_eq!(reply.session_id.as_deref(), Some("abc-123"));
        assert_eq!(reply.cost_usd, None);
    }

    #[test]
    fn parse_json_reply_falls_back_to_raw_text_on_malformed_json() {
        let reply = parse_json_reply("not json at all");
        assert_eq!(reply.text, "not json at all");
        assert_eq!(reply.session_id, None);
        assert_eq!(reply.cost_usd, None);
    }

    #[cfg(unix)]
    #[test]
    fn run_headless_with_request_json_parses_a_json_reply_from_the_child() {
        use std::os::unix::fs::PermissionsExt;
        // This file's usual `sh -c <script>` stub-agent idiom doesn't work
        // with `request_json: true`: it prepends `--output-format json`
        // ahead of `extra_args`, and a real `sh`/bash rejects that as an
        // unrecognized long option before ever reaching `-c` -- so instead
        // this fake binary ignores its argv entirely and always emits a
        // fixed JSON reply. It lives under this crate's own `target/` dir,
        // not the system temp dir -- `run_headless`'s bwrap sandbox remounts
        // `/tmp` as a private, empty tmpfs invisible to the child (see
        // `flare_sandbox::bwrap`'s `--tmpfs /tmp`), while `target/` sits
        // under the sandboxed cwd bind and stays visible and writable.
        let dir = tempfile::Builder::new()
            .prefix("agentflare-fake-claude-")
            .tempdir_in(std::env::current_dir().unwrap().join("target"))
            .unwrap();
        let fake_bin = dir.path().join("fake-claude");
        std::fs::write(
            &fake_bin,
            "#!/bin/sh\necho '{\"type\":\"result\",\"result\":\"pong\",\"session_id\":\"sess-xyz\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let binary_names: &'static [&'static str] = Box::leak(
            vec![&*Box::leak(
                fake_bin.to_string_lossy().into_owned().into_boxed_str(),
            )]
            .into_boxed_slice(),
        );

        let reg = vec![AgentSpec {
            id: Agent::ClaudeCode,
            display_name: "claude-code",
            tier: Tier::Cli,
            binary_names,
            version_args: &[],
            package_manager: None,
            package_name: None,
        }];
        match run_headless(
            &reg,
            "claude-code",
            "hi",
            Duration::from_secs(10),
            Duration::from_secs(10),
            &[],
            true,
        ) {
            HeadlessOutcome::Ok(reply) => {
                assert_eq!(reply.text, "pong");
                assert_eq!(reply.session_id.as_deref(), Some("sess-xyz"));
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }
