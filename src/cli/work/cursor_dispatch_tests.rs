use super::*;

#[test]
fn build_extra_args_gives_cursor_stream_json_but_no_claude_only_flags() {
    // item #183: cursor-agent needs the same idle-timeout fix Claude Code
    // got under item #43/#489, but --verbose/--max-turns/--max-cost-usd
    // stay Claude-Code-only, unconfirmed for cursor-agent.
    let args = build_extra_args(agent_registry::Agent::Cursor, Some(5), Some(1.0), None);
    assert_eq!(
        args,
        vec![
            "--force".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
        ]
    );
}
