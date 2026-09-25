// Agent-exhaustion failover + stop-on-request tests. Included from
// `work::tests` via `include!`.

/// A test project/item plus a fresh temp home, so `auth_db` cooldowns and
/// the backend are both isolated.
fn with_failover_fixture(f: impl FnOnce(&AgentflareMcp, &agentflare_backend::item::Item)) {
    with_failover_fixture_claimed(true, f);
}

/// [`with_failover_fixture`], optionally with the item claimed by this
/// process's owner (as the real `work` run holds it) or left unclaimed.
fn with_failover_fixture_claimed(
    claimed: bool,
    f: impl FnOnce(&AgentflareMcp, &agentflare_backend::item::Item),
) {
    crate::paths::test_support::with_temp_home(|| {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        init_test_repo(&repo_root);
        let mcp = AgentflareMcp::for_test(
            tmp.path().join("backend.db"),
            repo_root,
            tmp.path().join("project.json"),
        );
        let item = mcp.with_backend_db(|conn| seeded_item(&mcp, conn)).unwrap();
        if claimed {
            mcp.with_backend_db(|conn| {
                agentflare_backend::claim::acquire(
                    conn,
                    &item.id,
                    &crate::claims::owner_id(),
                    crate::claims::now(),
                    3600,
                )
            })
            .unwrap()
            .unwrap();
        }
        // One dispatch cycle, as the supervisor would have recorded it.
        mcp.with_backend_db(|conn| {
            agentflare_backend::comment::create(
                conn,
                &item.id,
                "supervisor",
                &format!(
                    "{}\n\njob: j1",
                    crate::dispatch_failure_ceiling::DISPATCH_MARKER
                ),
            )
        })
        .unwrap()
        .unwrap();
        // Comment order is by second-resolution timestamp: keep the
        // outcome comments the test produces strictly after the marker.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        f(&mcp, &item);
    });
}

fn comments_of(
    mcp: &AgentflareMcp,
    item_id: &str,
) -> Vec<agentflare_backend::comment::ItemComment> {
    mcp.with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, item_id))
        .unwrap()
        .unwrap()
}

const CREDIT_OUT: &str = "Workflow failed at step sdd_loop: Claude Code exited non-zero — last stdout before kill:\n{\"type\":\"result\",\"is_error\":true,\"result\":\"Credit balance is too low\"}";

#[test]
fn credit_exhaustion_fails_over_to_the_next_available_agent_without_counting() {
    with_failover_fixture(|mcp, item| {
        let mut guard = ClaimGuard::new(mcp, &item.id);
        let mut log = Vec::new();
        let outcome = handle_agent_exhaustion(
            mcp,
            item,
            &[],
            agent_registry::Agent::ClaudeCode,
            CREDIT_OUT,
            true,
            &mut guard,
            None,
            &mut log,
            |_, _, exclude| {
                assert_eq!(exclude, agent_registry::Agent::ClaudeCode);
                Some(agent_registry::Agent::Codex)
            },
        )
        .expect("credit exhaustion is handled");
        assert!(outcome.fatal, "the failed job ends; the item moves on");
        assert_eq!(outcome.retry_after_secs, None);
        assert!(!guard.armed, "claim already released");

        let moved = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item.id))
            .unwrap()
            .unwrap();
        assert_eq!(
            moved.assignee_agent.as_deref(),
            Some("codex"),
            "sticky reassignment"
        );

        let comments = comments_of(mcp, &item.id);
        assert!(comments.iter().any(|c| {
            c.body
                .starts_with(crate::dispatch_failure_ceiling::AGENT_FAILOVER_MARKER)
                && c.body.contains("moved from claude-code to codex")
        }));
        assert!(
            !comments.iter().any(|c| c
                .body
                .starts_with(crate::dispatch_failure_ceiling::WORK_FAILURE_MARKER)),
            "no failure comment: the item didn't fail"
        );
        assert_eq!(
            crate::dispatch_failure_ceiling::consecutive_failure_count_any_reason(&comments),
            0
        );
        assert_eq!(
            crate::dispatch_failure_ceiling::consecutive_identical_failure_count(&comments),
            0
        );
        assert!(
            crate::quota::failover::unavailable_until("claude-code").is_some(),
            "the exhausted agent is recorded unavailable"
        );
    });
}

#[test]
fn no_alternative_schedules_the_retry_at_the_reset_time_without_counting() {
    with_failover_fixture(|mcp, item| {
        let mut guard = ClaimGuard::new(mcp, &item.id);
        let mut log = Vec::new();
        let msg = "codex exited non-zero — last stderr before kill:\nYou've hit your usage limit. Try again in 3 hours.";
        let outcome = handle_agent_exhaustion(
            mcp,
            item,
            &[],
            agent_registry::Agent::Codex,
            msg,
            true,
            &mut guard,
            None,
            &mut log,
            |_, _, _| None,
        )
        .expect("quota exhaustion is handled");
        assert!(!outcome.fatal, "retried later on the same agent");
        let retry = outcome.retry_after_secs.expect("retry scheduled");
        assert!(
            (3 * 3600 - 5..=3 * 3600).contains(&retry),
            "retry at the printed reset, not a blind 1800s: {retry}"
        );
        let (until, _) = crate::quota::failover::unavailable_until("codex").unwrap();
        assert!((until - (chrono::Utc::now().timestamp() + 3 * 3600)).abs() <= 120);

        let comments = comments_of(mcp, &item.id);
        assert!(comments.iter().any(|c| {
            c.body
                .starts_with(crate::dispatch_failure_ceiling::AGENT_UNAVAILABLE_MARKER)
        }));
        assert_eq!(
            crate::dispatch_failure_ceiling::consecutive_failure_count_any_reason(&comments),
            0,
            "waiting for a reset must not burn the failure ceiling"
        );
    });
}

#[test]
fn failover_off_waits_instead_of_moving() {
    with_failover_fixture(|mcp, item| {
        let mut guard = ClaimGuard::new(mcp, &item.id);
        let mut log = Vec::new();
        let outcome = handle_agent_exhaustion(
            mcp,
            item,
            &[],
            agent_registry::Agent::ClaudeCode,
            CREDIT_OUT,
            false,
            &mut guard,
            None,
            &mut log,
            |_, _, _| panic!("failover disabled: must not look for an alternative"),
        )
        .unwrap();
        assert!(!outcome.fatal);
        assert_eq!(
            outcome.retry_after_secs,
            Some(crate::auth_runner::CREDIT_EXHAUSTED_SECS)
        );
    });
}

#[test]
fn short_rate_limit_retries_the_same_agent() {
    with_failover_fixture(|mcp, item| {
        let mut guard = ClaimGuard::new(mcp, &item.id);
        let mut log = Vec::new();
        let outcome = handle_agent_exhaustion(
            mcp,
            item,
            &[],
            agent_registry::Agent::ClaudeCode,
            "API Error: 429 Too Many Requests. Retry-After: 30",
            true,
            &mut guard,
            None,
            &mut log,
            |_, _, _| panic!("a short rate limit must not fail over"),
        )
        .unwrap();
        assert!(!outcome.fatal);
        assert_eq!(outcome.retry_after_secs, Some(30));
        let comments = comments_of(mcp, &item.id);
        assert!(comments.iter().any(|c| {
            c.body
                .starts_with(crate::dispatch_failure_ceiling::AGENT_UNAVAILABLE_MARKER)
        }));
        assert!(
            !comments.iter().any(|c| c.body.contains("agentflare work — failed")),
            "a deliberate short retry must not post a failure comment"
        );
        assert_eq!(
            crate::dispatch_failure_ceiling::consecutive_failure_count_any_reason(&comments),
            0,
            "a short rate-limit retry must not burn the failure ceiling"
        );
    });
}

#[test]
fn non_exhaustion_failures_fall_through() {
    with_failover_fixture(|mcp, item| {
        let mut guard = ClaimGuard::new(mcp, &item.id);
        let mut log = Vec::new();
        assert!(
            handle_agent_exhaustion(
                mcp,
                item,
                &[],
                agent_registry::Agent::ClaudeCode,
                "judge reply was not valid JSON",
                true,
                &mut guard,
                None,
                &mut log,
                |_, _, _| panic!("not exhaustion"),
            )
            .is_none()
        );
        assert!(crate::quota::failover::unavailable_until("claude-code").is_none());
    });
}

#[test]
fn stop_on_request_classification() {
    assert_eq!(
        stop_on_request(&format!("step failed: {PAUSED_MESSAGE}"), false),
        Some(StopOnRequest::Paused)
    );
    assert_eq!(
        stop_on_request(agentflare_jobs::cancel::CANCELLED_MESSAGE, false),
        Some(StopOnRequest::JobCancelled)
    );
    assert_eq!(
        stop_on_request("workflow run failed", true),
        Some(StopOnRequest::RunCancelled)
    );
    assert_eq!(
        stop_on_request("Workflow cancelled for item-1", false),
        Some(StopOnRequest::RunCancelled)
    );
    assert_eq!(stop_on_request("workflow run failed", false), None);
}

#[test]
fn a_run_already_cancelled_before_the_dispatch_is_not_its_cancellation() {
    // Resumed run cancelled mid-dispatch.
    assert!(run_cancelled_by_this_dispatch(None, Some("run-1")));
    // Stale cancelled run replaced by a fresh one, which was then cancelled.
    assert!(run_cancelled_by_this_dispatch(Some("run-1"), Some("run-2")));
    // Setup error before the fresh run's id was persisted: the stale
    // cancelled id is still stored -- a failure, not a cancel-on-request.
    assert!(!run_cancelled_by_this_dispatch(Some("run-1"), Some("run-1")));
    assert!(!run_cancelled_by_this_dispatch(Some("run-1"), None));
    assert!(!run_cancelled_by_this_dispatch(None, None));
}

#[test]
fn a_cancelled_or_paused_run_is_terminal_and_not_counted() {
    with_failover_fixture(|mcp, item| {
        for stop in [StopOnRequest::RunCancelled, StopOnRequest::Paused] {
            let mut guard = ClaimGuard::new(mcp, &item.id);
            let mut log = Vec::new();
            let outcome = end_stopped_run(mcp, &item.id, stop, &mut guard, &mut log);
            let failure = job_failure_for(&outcome);
            assert!(
                failure.fatal,
                "{stop:?} must never be re-queued by the job queue"
            );
            assert_eq!(failure.retry_after_secs, None);
            let comments = comments_of(mcp, &item.id);
            assert!(
                crate::dispatch_failure_ceiling::stopped_on_request(&comments),
                "the terminal-job hook must see the stop and not re-arm ready-for-work"
            );
            assert_eq!(
                crate::dispatch_failure_ceiling::consecutive_failure_count_any_reason(&comments),
                0
            );
        }
    });
}

#[test]
fn release_response_released_only_trusts_an_explicit_true() {
    assert!(release_response_released(r#"{"released":true,"item_id":"x"}"#));
    assert!(!release_response_released(r#"{"released":false,"item_id":"x"}"#));
    assert!(!release_response_released(r#"{"item_id":"x"}"#));
    assert!(!release_response_released("not json"));
}

#[test]
fn a_no_op_release_keeps_the_claim_guard_armed_and_does_not_fail_over() {
    // Nothing to release (not our claim): `item_release` answers
    // `Ok({"released": false})`, which must not disarm the guard.
    with_failover_fixture_claimed(false, |mcp, item| {
        let mut guard = ClaimGuard::new(mcp, &item.id);
        let mut log = Vec::new();
        let outcome = handle_agent_exhaustion(
            mcp,
            item,
            &[],
            agent_registry::Agent::ClaudeCode,
            CREDIT_OUT,
            true,
            &mut guard,
            None,
            &mut log,
            |_, _, _| panic!("an unreleased claim must not fail over"),
        )
        .expect("credit exhaustion is handled");
        assert!(!outcome.fatal);
        assert!(guard.armed, "a no-op release must not disarm the guard");
        guard.disarm();
    });
}
