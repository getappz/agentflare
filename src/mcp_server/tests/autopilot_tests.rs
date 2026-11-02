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
    // A live agent session may rewrite this file, so prove isolation by the
    // unique session id never appearing in it, not by mtime.
    let real_path = dirs::home_dir()
        .unwrap()
        .join(".agentflare")
        .join("runtime-state.json");
    let (_tmp, s) = harness();
    let repo = init_temp_repo("task/x");
    let sid = uniq_session("iso");
    crate::paths::test_support::with_temp_home(|| {
        observe(
            &s,
            &sid,
            repo.path(),
            "ReportFindings",
            json!({}),
            None,
            "{}",
        );
        let temp_path = crate::optimize::runtime_state_path();
        assert_ne!(temp_path, real_path);
        assert!(std::fs::read_to_string(&temp_path).unwrap().contains(&sid));
    });
    let real = std::fs::read_to_string(&real_path).unwrap_or_default();
    assert!(
        !real.contains(&sid),
        "test leaked into the real runtime state"
    );
}

fn briefing_in(s: &AgentflareMcp, etag: Option<&str>, cwd: &std::path::Path) -> serde_json::Value {
    let out = s
        .autopilot_impl(AutopilotRequest {
            action: "briefing".into(),
            cwd: Some(cwd.display().to_string()),
            agent: Some("claude-code".into()),
            etag: etag.map(Into::into),
            ..Default::default()
        })
        .unwrap();
    serde_json::from_str(&out).unwrap()
}

fn briefing(s: &AgentflareMcp, etag: Option<&str>) -> serde_json::Value {
    briefing_in(s, etag, std::env::temp_dir().as_path())
}

#[test]
fn briefing_returns_text_and_stable_etag() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        briefing(&s, None); // first call may apply one-time component setup lines
        let a = briefing(&s, None);
        let b = briefing(&s, None);
        assert_eq!(a["etag"], b["etag"]);
        assert!(a["text"].as_str().unwrap().contains("agentflare"));
    });
}

#[test]
fn briefing_with_matching_etag_is_unchanged() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        briefing(&s, None); // first call may apply one-time component setup lines
        let a = briefing(&s, None);
        let b = briefing(&s, a["etag"].as_str());
        assert_eq!(b["unchanged"], true);
        assert!(b.get("text").is_none());
    });
}

#[test]
fn briefing_names_the_active_claim_for_cwd() {
    crate::paths::test_support::with_temp_home(|| {
        let (s, _tmp, _repo, item_id, _project, wt_path) = super::mcp_with_claimed_item("brief");
        let out = briefing_in(&s, None, &wt_path);
        assert!(out["text"].as_str().unwrap().contains(&item_id), "{out}");
    });
}

fn checkpoint_req(session: Option<&str>, payload: Option<serde_json::Value>) -> AutopilotRequest {
    AutopilotRequest {
        action: "checkpoint".into(),
        session_id: session.map(Into::into),
        payload,
        ..Default::default()
    }
}

fn full_payload() -> serde_json::Value {
    json!({
        "item": {"id": "abc-123", "seq": 42, "title": "Do the thing"},
        "branch": "feat/x",
        "worktree": "/w/x",
        "verification_fresh": true,
        "review_fresh": false,
        "decisions": ["use sqlite"],
        "remaining": ["write docs"]
    })
}

#[test]
fn checkpoint_without_payload_or_session_is_invalid_params() {
    let (_tmp, s) = harness();
    for req in [
        checkpoint_req(Some("s"), None),
        checkpoint_req(None, Some(full_payload())),
    ] {
        let err = s.autopilot_impl(req).unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }
}

#[test]
fn checkpoint_text_lists_item_branch_gates_decisions_and_remaining() {
    let p: super::super::autopilot::CheckpointPayload =
        serde_json::from_value(full_payload()).unwrap();
    let t = super::super::autopilot::checkpoint_text(&p);
    assert!(t.starts_with("[agentflare checkpoint]"), "{t}");
    for needle in [
        "#42",
        "feat/x",
        "/w/x",
        "tests fresh",
        "review stale",
        "use sqlite",
        "write docs",
    ] {
        assert!(t.contains(needle), "missing {needle} in {t}");
    }
}

#[test]
fn checkpoint_stores_handoff_and_returns_text() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let sid = uniq_session("ckpt");
        let out: serde_json::Value = serde_json::from_str(
            &s.autopilot_impl(checkpoint_req(Some(&sid), Some(full_payload())))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(out["handoff_id"], sid.as_str());
        assert!(
            out["text"]
                .as_str()
                .unwrap()
                .starts_with("[agentflare checkpoint]")
        );
    });
}

#[test]
fn checkpoint_still_returns_text_when_memory_write_fails() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        // A directory where brain.db should be makes the memory open fail.
        std::fs::create_dir_all(crate::memory::store::brain_db_path()).unwrap();
        let out: serde_json::Value = serde_json::from_str(
            &s.autopilot_impl(checkpoint_req(Some("s-fail"), Some(full_payload())))
                .unwrap(),
        )
        .unwrap();
        assert!(out["handoff_id"].is_null(), "{out}");
        assert!(
            out["text"]
                .as_str()
                .unwrap()
                .starts_with("[agentflare checkpoint]")
        );
    });
}

fn checkpoint_text_for(sid: &str, cwd: &std::path::Path, payload: serde_json::Value) -> String {
    let (_tmp, s) = harness();
    let mut req = checkpoint_req(Some(sid), Some(payload));
    req.cwd = Some(cwd.display().to_string());
    req.agent = Some("claude-code".into());
    let out: serde_json::Value = serde_json::from_str(&s.autopilot_impl(req).unwrap()).unwrap();
    out["text"].as_str().unwrap().to_string()
}

#[test]
fn checkpoint_explicit_flags_win_over_the_session_record() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let repo = init_temp_repo("task/x");
        let sid = uniq_session("ckpt-explicit");
        observe(
            &s,
            &sid,
            repo.path(),
            "Bash",
            json!({"command":"cargo test"}),
            Some(0),
            "test result: ok. 1 passed",
        );
        let t = checkpoint_text_for(
            &sid,
            repo.path(),
            json!({"verification_fresh": false, "review_fresh": true}),
        );
        assert!(t.contains("tests stale, review fresh"), "{t}");
    });
}

#[test]
fn checkpoint_without_flags_derives_freshness_from_the_session_record() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let repo = init_temp_repo("task/x");
        let sid = uniq_session("ckpt-derive");
        observe(
            &s,
            &sid,
            repo.path(),
            "Bash",
            json!({"command":"cargo test"}),
            Some(0),
            "test result: ok. 1 passed",
        );
        let t = checkpoint_text_for(&sid, repo.path(), json!({}));
        assert!(t.contains("tests fresh, review stale"), "{t}");
    });
}

#[test]
fn checkpoint_without_flags_or_record_is_stale() {
    crate::paths::test_support::with_temp_home(|| {
        let repo = init_temp_repo("task/x");
        let t = checkpoint_text_for(&uniq_session("ckpt-none"), repo.path(), json!({}));
        assert!(t.contains("tests stale, review stale"), "{t}");
    });
}
