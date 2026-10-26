use super::tests::{
    coderabbit_finding, seed_gate_label, seed_in_review_item_with_claim_age, test_mcp, test_queue,
};
use super::*;
use crate::github::{
    RepoId,
    models::PullRequest,
    pulls,
    test_support::{MockResponse, MockServer},
};

fn pr(branch: &str, title: &str, body: Option<&str>) -> PullRequest {
    serde_json::from_value(serde_json::json!({
        "number": 870, "html_url": "u", "state": "open", "title": title,
        "body": body, "head": {"ref": branch}
    }))
    .unwrap()
}

#[test]
fn item724_checks_identity_without_breaking_renamed_stamped_tasks() {
    let mcp = test_mcp();
    let id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1900);
    let mut item = mcp
        .with_backend_db(|c| agentflare_backend::item::get(c, &id).unwrap())
        .unwrap();
    item.metadata = "{}".into();
    item.name = "Repair worktree removal".into();
    let branch = format!("task/{}-unrelated-failover", item.sequence_id);
    let mut pr = pr(&branch, "fix: failover thresholds", None);
    assert!(crate::worktree::pr_item_mismatch(&item, &pr).is_some());
    pr.title = "fix: Repair worktree removal (#870)".into();
    assert!(crate::worktree::pr_item_mismatch(&item, &pr).is_none());
    pr.title = "renamed task".into();
    pr.body = Some(pulls::item_id_tag(&item.id));
    assert!(crate::worktree::pr_item_mismatch(&item, &pr).is_none());
    item.metadata = r#"{"pr":{"number":999}}"#.into();
    assert!(crate::worktree::pr_item_mismatch(&item, &pr).is_some());
    item.metadata = "{}".into();
    pr.body = Some(pulls::origin_tag(
        &crate::github::bridge::config::stable_instance_id(),
        "another-item",
        item.sequence_id,
        &branch,
    ));
    assert!(crate::worktree::pr_item_mismatch(&item, &pr).is_some());
    pr.body = Some(format!(
        "{}\n{}",
        pulls::item_id_tag(&item.id),
        pulls::item_id_tag("other")
    ));
    assert!(crate::worktree::pr_item_mismatch(&item, &pr).is_some());
}

#[test]
fn item724_bare_numeric_branches_need_title_identity() {
    let mcp = test_mcp();
    let id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1900);
    let mut item = mcp
        .with_backend_db(|c| agentflare_backend::item::get(c, &id).unwrap())
        .unwrap();
    item.metadata = "{}".into();
    item.name = "你好".into();
    let branch = flare_git_core::worktree::task_branch_name(&item);
    assert_eq!(branch, format!("task/{}", item.sequence_id));
    assert!(crate::worktree::pr_item_mismatch(&item, &pr(&branch, "unrelated", None)).is_some());
    assert!(crate::worktree::pr_item_mismatch(&item, &pr(&branch, &item.name, None)).is_none());
}

#[test]
fn item724_explicit_adoption_preserves_source_identity_and_allows_renames() {
    let mcp = test_mcp();
    let id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1900);
    let mut item = mcp
        .with_backend_db(|c| agentflare_backend::item::get(c, &id).unwrap())
        .unwrap();
    for with_origin in [true, false] {
        let mut body = pulls::item_id_tag("source-item");
        if with_origin {
            body.push_str(&pulls::origin_tag(
                "old-instance",
                "source-item",
                308,
                "task/308-source",
            ));
        }
        let mut source = pr("task/308-source", "source title", Some(&body));
        let mut meta = crate::mcp_server::metadata_object(
            r#"{"pr":{"number":870,"branch":"task/308-source"},"keep":1}"#,
        );
        crate::worktree::record_adopted_identity(&mut meta, &source);
        assert_eq!(meta["keep"], 1);
        item.metadata = serde_json::to_string(&meta).unwrap();
        if with_origin {
            assert!(
                crate::worktree::pr_item_mismatch(&item, &source).is_some(),
                "adoption still requires local instance ownership"
            );
            source.body =
                pulls::restamp_origin(&body, &crate::github::bridge::config::stable_instance_id());
        }
        source.head.as_mut().unwrap().git_ref = "renamed-branch".into();
        source.title = "renamed title".into();
        assert!(crate::worktree::pr_item_mismatch(&item, &source).is_none());
        source.body = Some(pulls::item_id_tag("different-source"));
        assert!(crate::worktree::pr_item_mismatch(&item, &source).is_some());
        source.number = 871;
        assert!(crate::worktree::pr_item_mismatch(&item, &source).is_some());
    }
    assert!(
        pulls::item_id_of(Some(&format!(
            "{}{}",
            pulls::item_id_tag("one"),
            pulls::item_id_tag("two")
        )))
        .is_none()
    );
}

#[test]
fn item724_mismatched_live_pr_gates_without_reading_threads_or_dispatching() {
    let mcp = test_mcp();
    let queue = test_queue();
    let id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1900);
    let labels = seed_gate_label(&mcp);
    let item = mcp
        .with_backend_db(|c| agentflare_backend::item::get(c, &id).unwrap())
        .unwrap();
    let body = serde_json::json!({
        "number": 870, "html_url": "u", "state": "open", "title": "unrelated failover",
        "head": {"ref": format!("task/{}-unrelated-failover", item.sequence_id)}
    })
    .to_string();
    let server = MockServer::start_with(move |_| MockResponse::json(200, &body));
    let repo = RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };
    for _ in 0..2 {
        let state = validated_review_bot_state(
            &server.client(Some("tok")),
            &repo,
            &mcp,
            &queue,
            &ReviewBotConfig::default(),
            ReviewSweepInput {
                item: &item,
                number: 870,
                head_sha: None,
                labels: &[],
                label_id_by_name: &labels,
                folder_path: "/repo",
            },
        );
        assert!(state.blocks_merge());
        assert!(!state.dispatch_needed());
        let auto_merge = crate::worktree::AutoMergeRef::default();
        let outcome = merge_or_repair_findings(
            &mcp,
            &queue,
            &super::tests::test_auth_conn(),
            agentflare_resource_gate::Policy::Normal,
            &item,
            std::path::Path::new("/repo"),
            870,
            &state,
            &[],
            &labels,
            "/repo",
            CiGreenMerge::Allowed {
                head_sha: None,
                auto_merge: &auto_merge,
            },
        );
        assert!(matches!(outcome, PassingPrOutcome::NotMerged));
        assert!(!job_in_flight(&queue, &id));
    }
    assert!(item_gated(&mcp, &item, &labels));
    let comments = mcp
        .with_backend_db(|c| agentflare_backend::comment::list_by_item(c, &id).unwrap())
        .unwrap();
    assert_eq!(comments.len(), 1, "mapping explanation is posted once");
    assert!(comments[0].body.contains("Verify the PR-to-item mapping"));
    assert!(
        server
            .requests()
            .iter()
            .all(|r| r.method == "GET" && r.path == "/repos/o/r/pulls/870")
    );
}

#[test]
fn item724_brief_separates_foreign_paths_and_keeps_renames_in_scope() {
    let mut findings: Vec<_> = (1..=3)
        .map(|id| coderabbit_finding(id, "coderabbitai[bot]"))
        .collect();
    findings[1].path = "src/foreign.rs".into();
    findings[2].path = "src/old.rs".into();
    let files =
        serde_json::from_value::<Vec<crate::github::models::PullRequestFile>>(serde_json::json!([
            {"filename":"src/lib.rs"}, {"filename":"src/new.rs", "previous_filename":"src/old.rs"}
        ]))
        .unwrap();
    let before = coderabbit_findings_fingerprint(&findings);
    mark_finding_scope(&mut findings, &files);
    assert_ne!(
        before,
        coderabbit_findings_fingerprint(&findings),
        "scope changes must refresh a silent retry's brief"
    );
    assert_eq!(
        findings.iter().map(|f| f.outside_diff).collect::<Vec<_>>(),
        [false, true, false]
    );
    let rendered = render_fix_task(&findings.iter().collect::<Vec<_>>(), 870, "item", 3);
    assert_eq!(
        rendered
            .matches("Do not edit it; report this thread as out_of_scope")
            .count(),
        1
    );
    let scope = rendered.find("Scope: the following finding").unwrap();
    let foreign = rendered.find("thread=\"PRRT_2\"").unwrap();
    assert!(scope < foreign);
    assert!(rendered.contains("Report every thread"));
}

#[test]
fn item724_fully_reported_noops_do_not_exhaust_the_repair_cap() {
    let mcp = test_mcp();
    let queue = test_queue();
    let id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1900);
    let labels = seed_gate_label(&mcp);
    let auth = super::tests::test_auth_conn();
    for n in 0..=crate::quota::decide::SELF_REPAIR_CAP {
        let item = mcp
            .with_backend_db(|c| agentflare_backend::item::get(c, &id).unwrap())
            .unwrap();
        let finding = coderabbit_finding(n as u64, "coderabbitai[bot]");
        let thread_id = finding.thread_id.clone();
        assert!(matches!(
            coderabbit_repair_or_gate(
                &mcp,
                &queue,
                &auth,
                agentflare_resource_gate::Policy::Normal,
                &item,
                870,
                &[finding],
                &[],
                &labels,
                "/repo",
                None,
            ),
            SelfRepairOutcome::Dispatched
        ));
        let jobs = queue.list(Some(agentflare_jobs::JobState::Queued)).unwrap();
        assert_eq!(jobs.len(), 1);
        crate::claims::with_owner_override(format!("claude-code:{}", jobs[0].id), || {
            mcp.item_review_result(ItemRequest {
                action: "review_result".into(),
                id: Some(id.clone()),
                thread_id: Some(thread_id),
                outcome: Some("out_of_scope".into()),
                note: Some("outside this PR diff".into()),
                ..Default::default()
            })
            .unwrap();
        });
        queue.cancel_for_item(&id, |_| false).unwrap();
        // Count from fresh metadata even though this sweep's item snapshot predates the report.
        assert!(!coderabbit_repair_capped_unresolved(&mcp, &item));
    }
    assert!(!item_gated(
        &mcp,
        &mcp.with_backend_db(|c| agentflare_backend::item::get(c, &id).unwrap())
            .unwrap(),
        &labels
    ));
}

#[test]
fn item724_scope_fingerprint_survives_long_bodies_and_large_batches() {
    let mut findings: Vec<_> = (1..=100)
        .map(|id| coderabbit_finding(id, "coderabbitai[bot]"))
        .collect();
    for finding in &mut findings {
        finding.body = "x".repeat(4000);
    }
    let inside = coderabbit_findings_fingerprint(&findings);
    findings.last_mut().unwrap().outside_diff = true;
    let outside = coderabbit_findings_fingerprint(&findings);
    assert_ne!(inside, outside);
    findings[50].outside_diff = true;
    let changed = coderabbit_findings_fingerprint(&findings);
    assert_ne!(changed, outside);
    findings.reverse();
    assert_eq!(changed, coderabbit_findings_fingerprint(&findings));
}
