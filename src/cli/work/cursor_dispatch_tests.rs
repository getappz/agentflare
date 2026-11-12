use super::*;

#[test]
fn build_extra_args_gives_cursor_no_claude_only_flags() {
    // --verbose/--max-turns/--max-cost-usd stay Claude-Code-only, unconfirmed
    // for cursor-agent. Its `stream-json` (item #183) comes per role from
    // `headless_full_args`, not from these shared args.
    let args = build_extra_args(agent_registry::Agent::Cursor, Some(5), Some(1.0), None);
    assert_eq!(
        args,
        vec![
            "--force".to_string(),
            "--trust".to_string(),
            "--approve-mcps".to_string(),
        ]
    );
}

#[test]
fn headless_full_args_gives_cursor_stream_json_when_json_is_requested() {
    let args = crate::agent_launch::headless_full_args(agent_registry::Agent::Cursor, true, &[]);
    assert_eq!(args, vec!["--output-format", "stream-json"]);
}
