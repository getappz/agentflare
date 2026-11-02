use super::*;

#[test]
fn autopilot_unknown_action_is_invalid_params() {
    let (_tmp, s) = harness();
    let err = s
        .autopilot_impl(AutopilotRequest {
            action: "nope".into(),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn autopilot_health_reports_mcp_version_and_daemon_state() {
    let (_tmp, s) = harness();
    let out: serde_json::Value = serde_json::from_str(
        &s.autopilot_impl(AutopilotRequest {
            action: "health".into(),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(out["mcp_version"], env!("CARGO_PKG_VERSION"));
    assert!(["up", "down", "unknown"].contains(&out["daemon"].as_str().unwrap()));
    assert!(out["stale_binary"].is_boolean());
}

#[test]
fn autopilot_is_listed_as_a_builtin_tool() {
    assert!(
        builtin_tools::search_builtin_tools("autopilot", 5, gateway_registry::MatchMode::Any)
            .iter()
            .any(|h| h.tool == "mcp__flare__autopilot")
    );
}

use crate::hook_redirect::test_support::init_temp_repo;
use serde_json::json;

fn check(
    s: &AgentflareMcp,
    tool: &str,
    input: serde_json::Value,
    cwd: &std::path::Path,
) -> serde_json::Value {
    let out = s
        .autopilot_impl(AutopilotRequest {
            action: "check".into(),
            session_id: Some("s1".into()),
            cwd: Some(cwd.display().to_string()),
            tool_name: Some(tool.into()),
            tool_input: Some(input),
            ..Default::default()
        })
        .unwrap();
    serde_json::from_str(&out).unwrap()
}

#[test]
fn check_denies_edit_on_default_branch_with_branch_guard_rule() {
    let (_tmp, s) = harness();
    let repo = init_temp_repo("master");
    let out = check(
        &s,
        "Edit",
        json!({"file_path": repo.path().join("a.rs")}),
        repo.path(),
    );
    assert_eq!(out["decision"], "deny");
    assert_eq!(out["rule"], "branch_guard");
}

#[test]
fn check_unwraps_gateway_ctx_patch_and_windows_backslash_path() {
    let (_tmp, s) = harness();
    let repo = init_temp_repo("master");
    let p = repo
        .path()
        .join("src")
        .join("x.rs")
        .display()
        .to_string()
        .replace('/', "\\");
    let out = check(
        &s,
        "mcp__flare__tool",
        json!({"action":"execute","server":"leanctx","tool":"ctx_patch","args":{"path": p}}),
        repo.path(),
    );
    assert_eq!(out["decision"], "deny");
    assert_eq!(out["rule"], "branch_guard");
}

#[test]
fn check_allows_read_tools_without_touching_guards() {
    let (_tmp, s) = harness();
    assert_eq!(
        check(
            &s,
            "Read",
            json!({"file_path":"/x"}),
            std::env::temp_dir().as_path()
        )["decision"],
        "allow"
    );
}

#[test]
fn check_denies_item_done_without_fresh_verification_with_completion_gate_rule() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let out = check(
            &s,
            "mcp__flare__item",
            json!({"action":"done","id":"abc"}),
            std::env::temp_dir().as_path(),
        );
        assert_eq!(out["decision"], "deny");
        assert_eq!(out["rule"], "completion_gate");
    });
}

#[test]
fn check_reason_strings_match_hook_path() {
    let (_tmp, s) = harness();
    let repo = init_temp_repo("master");
    let input = json!({"file_path": repo.path().join("a.rs")});
    let hook = crate::hook_redirect::redirect_decision("Edit", Some(&input)).unwrap();
    let out = check(&s, "Edit", input, repo.path());
    assert_eq!(
        out["reason"],
        hook["hookSpecificOutput"]["permissionDecisionReason"]
    );
}

/// Unique per run; callers run inside `with_temp_home`, so the runtime state is a temp file.
fn uniq_session(tag: &str) -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{tag}-{}-{n}", std::process::id())
}

fn observe(
    s: &AgentflareMcp,
    session: &str,
    cwd: &std::path::Path,
    tool: &str,
    input: serde_json::Value,
    exit_code: Option<i32>,
    output: &str,
) -> serde_json::Value {
    let out = s
        .autopilot_impl(AutopilotRequest {
            action: "observe".into(),
            session_id: Some(session.into()),
            cwd: Some(cwd.display().to_string()),
            agent: Some("claude-code".into()),
            tool_name: Some(tool.into()),
            tool_input: Some(input),
            exit_code,
            output_excerpt: Some(output.into()),
            ..Default::default()
        })
        .unwrap();
    serde_json::from_str(&out).unwrap()
}

fn check_done(s: &AgentflareMcp, session: &str, cwd: &std::path::Path) -> serde_json::Value {
    let out = s
        .autopilot_impl(AutopilotRequest {
            action: "check".into(),
            session_id: Some(session.into()),
            cwd: Some(cwd.display().to_string()),
            agent: Some("claude-code".into()),
            tool_name: Some("mcp__flare__item".into()),
            tool_input: Some(json!({"action":"done","id":"abc"})),
            ..Default::default()
        })
        .unwrap();
    serde_json::from_str(&out).unwrap()
}

#[test]
fn observe_passing_cargo_test_records_verification_then_only_review_is_missing() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let repo = init_temp_repo("task/x");
        let sid = uniq_session("obs-pass");
        let out = observe(
            &s,
            &sid,
            repo.path(),
            "Bash",
            json!({"command":"cargo test"}),
            Some(0),
            "test result: ok. 12 passed",
        );
        assert_eq!(out["recorded"], "verification");
        let chk = check_done(&s, &sid, repo.path());
        assert_eq!(chk["decision"], "deny");
        assert!(chk["reason"].as_str().unwrap().contains("review"), "{chk}");
    });
}

#[test]
fn observe_failing_test_is_recorded_as_a_failed_run_and_gate_stays_closed() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let repo = init_temp_repo("task/x");
        let sid = uniq_session("obs-fail");
        let out = observe(
            &s,
            &sid,
            repo.path(),
            "Bash",
            json!({"command":"cargo test"}),
            Some(1),
            "test result: FAILED",
        );
        assert_eq!(out["recorded"], "verification");
        let reason = check_done(&s, &sid, repo.path())["reason"]
            .as_str()
            .unwrap()
            .to_lowercase();
        assert!(
            reason.contains("verif") || reason.contains("test"),
            "{reason}"
        );
    });
}

#[test]
fn observe_unrelated_command_records_none() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let repo = init_temp_repo("task/x");
        let out = observe(
            &s,
            &uniq_session("obs-none"),
            repo.path(),
            "Bash",
            json!({"command":"ls"}),
            Some(0),
            "",
        );
        assert_eq!(out["recorded"], "none");
    });
}

#[test]
fn observe_report_findings_records_review() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let repo = init_temp_repo("task/x");
        let out = observe(
            &s,
            &uniq_session("obs-review"),
            repo.path(),
            "ReportFindings",
            json!({}),
            None,
            "{\"findings\":[]}",
        );
        assert_eq!(out["recorded"], "review");
    });
}

#[test]
fn observe_and_check_agree_on_the_request_cwd_tree() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let (a, b) = (init_temp_repo("task/a"), init_temp_repo("task/b"));
        std::fs::write(b.path().join("diff.txt"), "different tree").unwrap();
        let sid = uniq_session("obs-cwd");
        observe(
            &s,
            &sid,
            a.path(),
            "Bash",
            json!({"command":"cargo test"}),
            Some(0),
            "test result: ok. 1 passed",
        );
        observe(&s, &sid, a.path(), "ReportFindings", json!({}), None, "{}");
        // Same tree the evidence was recorded against: both gates satisfied.
        let same = check_done(&s, &sid, a.path());
        assert!(
            !same["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("review")
                && !same["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains("verif"),
            "{same}"
        );
        // A different tree must see that evidence as stale.
        let other = check_done(&s, &sid, b.path());
        assert_eq!(other["decision"], "deny", "{other}");
    });
}

#[test]
fn autopilot_tests_do_not_touch_the_real_runtime_state() {
    // Real path built straight from the OS home, bypassing the override.
    let real_path = dirs::home_dir()
        .unwrap()
        .join(".agentflare")
        .join("runtime-state.json");
    let snap = || {
        std::fs::metadata(&real_path)
            .ok()
            .map(|m| (m.len(), m.modified().ok()))
    };
    let before = snap();
    let (_tmp, s) = harness();
    let repo = init_temp_repo("task/x");
    crate::paths::test_support::with_temp_home(|| {
        observe(
            &s,
            &uniq_session("iso"),
            repo.path(),
            "ReportFindings",
            json!({}),
            None,
            "{}",
        );
        assert!(crate::optimize::runtime_state_path().exists());
        assert_ne!(crate::optimize::runtime_state_path(), real_path);
    });
    assert_eq!(snap(), before);
}
