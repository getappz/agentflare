// `build_extra_args`/`resolve_dispatch_model`/`resolve_agent` model-routing
// coverage, split out of work.rs (LOC gate, frozen at 2100) — same pattern
// as work_worktree_error_tests.rs. `include!`d into `mod tests` above, so it
// shares that module's imports/helpers verbatim.

#[test]
fn resolve_agent_surfaces_the_rule_configured_model() {
    let mut item = test_item();
    item.metadata = r#"{"size":"S"}"#.to_string();
    let config = agent_registry::parse_router_config(
        r#"
[router]
[[router.rule]]
when = { size = "S" }
use  = "opencode"
model = "sonnet"
"#,
    )
    .unwrap();
    let (_, _, _, model) = resolve_agent(
        None,
        &item,
        &[],
        &config,
        &[agent_registry::Agent::Opencode],
        None,
        &mut Default::default(),
    )
    .unwrap();
    assert_eq!(model, Some("sonnet".to_string()));
}

#[test]
fn build_extra_args_includes_bypass_and_streaming_output_for_claude() {
    // Plain `--output-format json` writes NOTHING to stdout/stderr until
    // the entire run finishes (confirmed by hand: 0 bytes for 54s+ on a
    // trivial 2-tool-call task) — run_captured's idle-timeout (default
    // 300s) then kills any real task before it can finish, every time.
    // `stream-json` (+ the `--verbose` it requires) emits one JSON
    // object per turn/tool-call as it happens, giving a genuine
    // liveness signal; its final line carries the same {"result":...}
    // shape `parse_claude_reply` already expects.
    let args = build_extra_args(agent_registry::Agent::ClaudeCode, None, None, None);
    assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(args.contains(&"--output-format".to_string()));
    assert!(args.contains(&"stream-json".to_string()));
    assert!(!args.contains(&"json".to_string()));
    assert!(args.contains(&"--verbose".to_string()));
    assert!(!args.iter().any(|a| a.starts_with("--max-turns")));
}

#[test]
fn build_extra_args_passes_through_max_turns_and_cost_for_claude() {
    let args = build_extra_args(agent_registry::Agent::ClaudeCode, Some(5), Some(2.5), None);
    assert!(args.contains(&"--max-turns=5".to_string()));
    assert!(args.contains(&"--max-budget-usd=2.5".to_string()));
}

#[test]
fn build_extra_args_for_codex_has_bypass_but_no_json_output() {
    let args = build_extra_args(agent_registry::Agent::Codex, None, None, None);
    assert_eq!(args, vec!["--full-auto".to_string()]);
}

#[test]
fn build_extra_args_passes_through_model_for_any_confirmed_agent() {
    let args = build_extra_args(
        agent_registry::Agent::Opencode,
        None,
        None,
        Some("anthropic/claude-sonnet-5"),
    );
    assert_eq!(
        args,
        vec![
            "--auto".to_string(),
            "--model".to_string(),
            "anthropic/claude-sonnet-5".to_string(),
        ]
    );
}

#[test]
fn build_extra_args_omits_model_flag_when_none() {
    let args = build_extra_args(agent_registry::Agent::Codex, None, None, None);
    assert!(!args.iter().any(|a| a == "--model"));
}

#[test]
fn resolve_dispatch_model_explicit_cli_flag_wins_over_router_model() {
    let model = resolve_dispatch_model(
        agent_registry::Agent::ClaudeCode,
        Some("opus"),
        Some("sonnet"),
        true,
    );
    assert_eq!(model, Some("opus".to_string()));
}

#[test]
fn resolve_dispatch_model_falls_back_to_router_model_when_no_cli_flag() {
    let model =
        resolve_dispatch_model(agent_registry::Agent::ClaudeCode, None, Some("sonnet"), true);
    assert_eq!(model, Some("sonnet".to_string()));
}

#[test]
fn resolve_dispatch_model_none_when_fallback_swapped_the_agent() {
    let model = resolve_dispatch_model(
        agent_registry::Agent::Opencode,
        Some("opus"),
        Some("sonnet"),
        false,
    );
    assert_eq!(model, None);
}

#[test]
fn resolve_dispatch_model_translates_claude_name_to_clinepass_for_cline() {
    let model = resolve_dispatch_model(agent_registry::Agent::Cline, None, Some("sonnet"), true);
    assert_eq!(model, Some("cline-pass/kimi-k2.7-code".to_string()));
}

#[test]
fn resolve_dispatch_model_passes_through_a_non_claude_name_for_cline_unchanged() {
    let model = resolve_dispatch_model(
        agent_registry::Agent::Cline,
        None,
        Some("cline-pass/glm-5.3"),
        true,
    );
    assert_eq!(model, Some("cline-pass/glm-5.3".to_string()));
}

#[test]
fn resolve_agent_explicit_flag_still_picks_up_a_matching_rules_model() {
    // Item #162: this path used to always return `model: None` without
    // ever consulting `[router]` rules. It's the path every normal
    // daemon dispatch takes (the daemon always passes an explicit
    // agent), so a rule's `model` never reached dispatch in practice.
    let item = test_item();
    let config = agent_registry::RouterConfig {
        default: None,
        rules: vec![agent_registry::RouterRule {
            when: agent_registry::RuleMatch {
                labels: vec!["urgent".to_string()],
                ..Default::default()
            },
            use_agents: vec![agent_registry::Agent::Opencode],
            rotate: false,
            model: Some("sonnet".to_string()),
        }],
    };
    let (agent, _, _, model) = resolve_agent(
        Some("cline"),
        &item,
        &["urgent".to_string()],
        &config,
        &[],
        None,
        &mut Default::default(),
    )
    .unwrap();
    assert_eq!(agent, agent_registry::Agent::Cline);
    assert_eq!(model.as_deref(), Some("sonnet"));
}
