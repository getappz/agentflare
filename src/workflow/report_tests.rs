use super::*;
use flare_insights::model::{Cost, Session, SessionSource, SessionStatus, TokenUsage};

fn at(second: u32) -> DateTime<Utc> {
    format!("2026-10-19T00:00:{second:02}Z").parse().unwrap()
}

fn state() -> Value {
    json!({
        "run_id": WorkflowRunId::new(),"workflow_id":"work-item","status":"completed",
        "created_at":at(10),"updated_at":at(20),"step_states":{},"input":"request","output":"done",
        "context":{"data":{"item_id":"item-1","worktree_path":"repo","agent_sessions":{"coder":"session-1"},"session_ids":["session-1"],"reply_text":"All checks passed"}},
    })
}

fn session(id: &str) -> Session {
    Session {
        id: id.into(),
        source: SessionSource::ClaudeCode,
        project: "project".into(),
        project_path: Some("repo".into()),
        title: None,
        model: Some("model".into()),
        status: SessionStatus::Completed,
        awaiting_reason: None,
        started_at: Some(at(0)),
        updated_at: at(30),
        ended_at: Some(at(30)),
        duration_secs: None,
        tokens: TokenUsage {
            input: 1,
            output: 2,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        cost: Some(Cost {
            total_usd: 3.5,
            input_usd: 0.0,
            output_usd: 0.0,
            cache_read_usd: 0.0,
            cache_write_usd: 0.0,
        }),
        turn_count: 0,
        tool_call_count: 0,
        subagent_count: 0,
        tags: vec![],
        starred: false,
        pid: None,
        cwd: Some("repo".into()),
    }
}

fn tool(id: &str, command: &str, output: Option<&str>) -> ToolCall {
    ToolCall {
        id: id.into(),
        session_id: "session-1".into(),
        turn_seq: 1,
        name: "exec_command".into(),
        input: json!({"cmd":command}),
        output: output.map(str::to_string),
        status: "completed".into(),
        duration_ms: None,
        created_at: Some(at(15)),
    }
}

#[test]
fn report_preserves_failed_and_successful_checks_without_trusting_agent_claims() {
    let store = InsightsStore::open_in_memory().unwrap();
    store.upsert_session(&session("session-1")).unwrap();
    for call in [
        tool(
            "failed",
            "cargo test",
            Some("Process exited with code 1\nOutput:\nassertion expected 2, got 1"),
        ),
        tool(
            "passed",
            "cargo test",
            Some("Process exited with code 0\nOutput:\n2 tests passed"),
        ),
        tool("unknown", "cargo fmt --check", Some("looks fine")),
        tool("pending", "cargo clippy", None),
    ] {
        store.upsert_tool_call(&call).unwrap();
    }
    let report = build_report(&state(), &[], Some(&store)).unwrap();
    let checks = report["sessions"][0]["observed_checks"].as_array().unwrap();
    assert_eq!(checks.len(), 4);
    assert_eq!(checks[0]["outcome"], "failed");
    assert!(
        checks[0]["output"]
            .as_str()
            .unwrap()
            .contains("expected 2, got 1")
    );
    assert_eq!(checks[1]["outcome"], "passed");
    assert_eq!(checks[2]["outcome"], "unknown");
    assert_eq!(checks[3]["outcome"], "unknown");
    assert_eq!(report["acceptance"]["status"], "unknown");
    assert_eq!(report["agent_claims"]["summary"], "All checks passed");
    assert_eq!(report["sessions"][0]["session_cost_usd"], 3.5);
    assert!(report["run_cost_usd"].is_null());
    // Tools without a retained parent turn still remain available.
    assert_eq!(
        report["sessions"][0]["events"]["tool_calls"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn report_excludes_events_outside_the_run_and_sessions_from_other_worktrees() {
    let store = InsightsStore::open_in_memory().unwrap();
    store.upsert_session(&session("session-1")).unwrap();
    for (id, time) in [
        ("before", Some(at(1))),
        ("within", Some(at(15))),
        ("after", Some(at(30))),
        ("undated", None),
    ] {
        let mut call = tool(
            id,
            "cargo test",
            Some("Process exited with code 0\nOutput:\nok"),
        );
        call.created_at = time;
        store.upsert_tool_call(&call).unwrap();
    }
    let report = build_report(&state(), &[], Some(&store)).unwrap();
    assert_eq!(
        report["sessions"][0]["observed_checks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        report["sessions"][0]["observed_checks"][0]["tool_call_id"],
        "within"
    );
    assert_eq!(
        report["sessions"][0]["coverage"]["tools_without_timestamp"],
        1
    );
    let mut wrong = session("session-1");
    wrong.cwd = Some("other-project".into());
    store.upsert_session(&wrong).unwrap();
    let report = build_report(&state(), &[], Some(&store)).unwrap();
    assert_eq!(report["sessions"][0]["availability"], "unverified");
    assert!(report["sessions"][0]["events"].is_null());
    let mut missing_provenance = state();
    missing_provenance["context"]["data"]["worktree_path"] = Value::Null;
    assert_eq!(
        build_report(&missing_provenance, &[], Some(&store)).unwrap()["sessions"][0]["availability"],
        "unverified"
    );
}

#[test]
fn report_retains_journal_failures_and_historical_session_links() {
    let mut state = state();
    state["context"]["data"]["session_ids"] = json!(["session-1", "older-session"]);
    let journal = vec![
        JournalEntry::StepRun {
            step_id: flare_workflow::StepId::new("check"),
            attempt: 1,
            result: Some(EntryResult::Failure {
                code: 2,
                message: "exact failure".into(),
                metadata: vec![],
            }),
        },
        JournalEntry::StepRun {
            step_id: flare_workflow::StepId::new("check"),
            attempt: 2,
            result: Some(EntryResult::success(
                &json!({"data":{"session_id":"journal-session"}}),
            )),
        },
    ];
    let report = build_report(&state, &journal, None).unwrap();
    assert_eq!(
        report["journal_evidence"][0]["failure"]["message"],
        "exact failure"
    );
    assert_eq!(report["journal_evidence"][1]["attempt"], 2);
    assert_eq!(report["sessions"].as_array().unwrap().len(), 3);
    assert!(report["run_cost_usd"].is_null());
    assert!(
        !report["coverage"]["warnings"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn report_redacts_structured_credentials_and_counts_text_truncation() {
    let mut state = state();
    let private = ["private", "value"].join("-");
    let another = ["another", "private"].join("-");
    state["input"] = json!({"api_key":private,"nested":{"password":another}});
    state["context"]["data"]["reply_text"] = json!("é".repeat(5000));
    let report = build_report(&state, &[], None).unwrap();
    let encoded = report.to_string();
    assert!(!encoded.contains("private-value"));
    assert!(!encoded.contains("another-private"));
    assert_eq!(report["run"]["input"]["api_key"], "[REDACTED]");
    assert!(
        report["coverage"]["truncated_text_fields"]
            .as_u64()
            .unwrap()
            > 0
    );
}

#[test]
fn encoded_structured_tool_output_is_redacted_before_rendering() {
    let private = ["opaque", "credential", "value"].join("-");
    let mut value =
        json!({"output": json!({"api_key":private,"nested":[{"password":private}]}).to_string()});
    sanitize(&mut value);
    assert!(!value.to_string().contains(&private));
    let decoded: Value = serde_json::from_str(value["output"].as_str().unwrap()).unwrap();
    assert_eq!(decoded["api_key"], "[REDACTED]");
    for boundary in ["\nOutput:\n", "\r\nOutput:\r\n"] {
        let payload = json!({"api_key":private}).to_string();
        let mut output = json!(format!("Process exited with code 0{boundary}{payload}"));
        sanitize(&mut output);
        assert!(!output.to_string().contains(&private));
        assert!(
            output
                .as_str()
                .unwrap()
                .contains("Process exited with code 0")
        );
    }
}

#[test]
fn forced_batch_refresh_replaces_legacy_codex_argument_strings() {
    let store = InsightsStore::open_in_memory().unwrap();
    store.upsert_session(&session("session-1")).unwrap();
    let mut call = tool(
        "old",
        "cargo test",
        Some("Process exited with code 0\nOutput:\nok"),
    );
    let normalized = call.input.clone();
    call.input = json!(normalized.to_string());
    store.upsert_tool_call(&call).unwrap();
    call.input = normalized;
    store.upsert_tool_calls_batch(&[call]).unwrap();
    let report = build_report(&state(), &[], Some(&store)).unwrap();
    assert_eq!(
        report["sessions"][0]["observed_checks"][0]["outcome"],
        "passed"
    );
}

#[test]
fn structured_gateway_exit_status_is_reused_and_background_is_unknown() {
    let mut call = tool(
        "gateway",
        "cargo test",
        Some(r#"{"structuredContent":{"exitCode":0},"content":[{"type":"text","text":"done"}]}"#),
    );
    call.name = "mcp__lean_ctx__ctx_shell".into();
    assert_eq!(verification(&call).unwrap()["outcome"], "passed");
    call.output =
        Some(r#"[{"type":"text","text":"Process exited with code 1\nOutput:\nfailed"}]"#.into());
    assert_eq!(verification(&call).unwrap()["outcome"], "failed");
    call.output=Some(r#"{"structuredContent":{"state":"completed","exitCode":0},"content":[{"type":"text","text":"[background:job completed]"}]}"#.into());
    assert_eq!(verification(&call).unwrap()["outcome"], "passed");
    call.output = Some(r#"[{"type":"text","text":"shell failed\n[exit:1]"}]"#.into());
    assert_eq!(verification(&call).unwrap()["outcome"], "failed");
    call.output = Some(r#"[{"type":"text","text":"{\"exit_code\":0}"}]"#.into());
    assert_eq!(verification(&call).unwrap()["outcome"], "passed");
    call.output = Some("[background:job running]".into());
    assert_eq!(verification(&call).unwrap()["outcome"], "unknown");
    call.output =
        Some(r#"{"isError":true,"content":[{"type":"text","text":"transport failed"}]}"#.into());
    assert_eq!(verification(&call).unwrap()["outcome"], "error");
}

#[test]
fn provider_id_lookup_is_exact_or_unambiguous_codex_uuid_suffix() {
    let store = InsightsStore::open_in_memory().unwrap();
    let id = "01a14e71-c5d7-7a83-ac8f-f1cb656c4561";
    let mut s = session(&format!("rollout-2026-10-19-{id}"));
    s.source = SessionSource::Codex;
    store.upsert_session(&s).unwrap();
    assert_eq!(
        store.get_session_by_provider_id(id).unwrap().unwrap().id,
        s.id
    );
    s.id = format!("rollout-2026-10-18-{id}");
    store.upsert_session(&s).unwrap();
    assert!(store.get_session_by_provider_id(id).unwrap().is_none());
    assert!(store.get_session_by_provider_id("*").unwrap().is_none());
}

#[tokio::test]
async fn report_reads_foreign_workflow_data_without_writing_or_creating_databases() {
    use flare_workflow::{StateStore, WorkflowId, WorkflowState};
    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct Payload {
        task: String,
    }
    impl flare_workflow::WorkflowData for Payload {
        fn workflow_type() -> &'static str {
            "fixture"
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("workflow.db");
    let insights = dir.path().join("missing-insights.db");
    let store = flare_workflow::sqlite_store::SqliteStore::open_file(&db).unwrap();
    let run = WorkflowRunId::new();
    store
        .save(WorkflowState::new(
            run,
            WorkflowId::new("fixture"),
            Payload {
                task: "task".into(),
            },
        ))
        .await
        .unwrap();
    let before = std::fs::read(&db).unwrap();
    let report = read_report(run, &db, &insights).unwrap();
    assert_eq!(report["run"]["workflow_id"], "fixture");
    assert_eq!(std::fs::read(&db).unwrap(), before);
    assert!(!insights.exists());
    assert!(InsightsStore::open_read_only(&insights).is_err());
    assert!(!insights.exists());
    let missing = dir.path().join("missing-workflows.db");
    assert!(read_report(run, &missing, &insights).is_err());
    assert!(!missing.exists());
}
