//! Item #639: gated `force` override + dead-claim auto-release.
use super::*;
use crate::mcp_server::item_force::{
    AUTO_RELEASE_MARKER, FORCE_OVERRIDE_MARKER, auto_release_dead_claims, branch_gate,
    caller_on_pushed_branch,
};

const RESCUER: &str = "claude-code:rescuer";

/// An item claimed by `claude-code:<job-id>` for a job in the returned queue,
/// plus an mcp whose repo root is a non-git dir (so the PR check soft-fails
/// to "unverified" instead of hitting GitHub).
fn foreign_claim_harness() -> (tempfile::TempDir, AgentflareMcp, String, String) {
    let (tmp, mut s) = harness();
    s.worktree_repo_root_override = Some(tmp.path().to_path_buf());
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let queue = agentflare_jobs::Queue::open_memory(tmp.path().join("job-logs")).unwrap();
    let job = queue
        .enqueue(
            &agentflare_jobs::AgentJob::new("agentflare-work")
                .args([item_id.as_str(), "claude-code"])
                .in_process(),
        )
        .unwrap();
    s.job_queue_override = Some(queue);
    let holder = format!("claude-code:{}", job.id);
    let conn = backend_conn(&tmp);
    let acquired = agentflare_backend::claim::acquire(
        &conn,
        &item_id,
        &holder,
        crate::claims::now(),
        backend_claim_ttl_secs(),
    )
    .unwrap();
    assert_eq!(acquired, agentflare_backend::claim::Acquire::Acquired);
    (tmp, s, item_id, job.id)
}

fn kill_job(s: &AgentflareMcp, job_id: &str) {
    s.job_queue_override
        .as_ref()
        .unwrap()
        .cancel(job_id)
        .unwrap();
}

fn post_failure(tmp: &tempfile::TempDir, item_id: &str) {
    agentflare_backend::comment::create(
        &backend_conn(tmp),
        item_id,
        "claude-code",
        "## agentflare work — failed\n\nreal commits but no PR resulted -- not marking completed",
    )
    .unwrap();
}

fn force_req(action: &str, item_id: &str, reason: Option<&str>) -> ItemRequest {
    ItemRequest {
        action: action.into(),
        id: Some(item_id.to_string()),
        force: Some(true),
        force_reason: reason.map(str::to_string),
        ..Default::default()
    }
}

fn holder_of(tmp: &tempfile::TempDir, item_id: &str) -> Option<String> {
    agentflare_backend::claim::current_owner(&backend_conn(tmp), item_id)
}

fn comments(tmp: &tempfile::TempDir, item_id: &str) -> Vec<String> {
    agentflare_backend::comment::list_by_item(&backend_conn(tmp), item_id)
        .unwrap()
        .into_iter()
        .map(|c| c.body)
        .collect()
}

#[test]
fn force_without_a_reason_is_rejected() {
    let (tmp, s, item_id, job_id) = foreign_claim_harness();
    kill_job(&s, &job_id);
    for reason in [None, Some("   ")] {
        let err = crate::claims::with_owner_override(RESCUER, || {
            s.item(Parameters(force_req("release", &item_id, reason)))
        })
        .unwrap_err();
        assert!(err.message.contains("force_reason"), "{err:?}");
    }
    assert_eq!(
        holder_of(&tmp, &item_id),
        Some(format!("claude-code:{job_id}"))
    );
}

#[test]
fn force_with_no_gate_condition_leaves_a_live_claim_alone() {
    let (tmp, s, item_id, job_id) = foreign_claim_harness();
    let err = crate::claims::with_owner_override(RESCUER, || {
        s.item(Parameters(force_req(
            "release",
            &item_id,
            Some("impatient"),
        )))
    })
    .unwrap_err();
    assert!(err.message.contains("force refused"), "{err:?}");
    assert_eq!(
        holder_of(&tmp, &item_id),
        Some(format!("claude-code:{job_id}"))
    );
    assert!(comments(&tmp, &item_id).is_empty());
}

#[test]
fn a_failure_comment_older_than_the_current_claim_is_not_evidence() {
    let (tmp, s, item_id, job_id) = foreign_claim_harness();
    post_failure(&tmp, &item_id);
    backend_conn(&tmp)
        .execute(
            "UPDATE item_comments SET created_at = created_at - 3600 WHERE item_id = ?1",
            [&item_id],
        )
        .unwrap();
    let err = crate::claims::with_owner_override(RESCUER, || {
        s.item(Parameters(force_req(
            "release",
            &item_id,
            Some("stale failure"),
        )))
    })
    .unwrap_err();
    assert!(err.message.contains("force refused"), "{err:?}");
    assert_eq!(
        holder_of(&tmp, &item_id),
        Some(format!("claude-code:{job_id}"))
    );
}

#[test]
fn force_release_of_a_dead_jobs_claim_succeeds_and_is_audited() {
    let (tmp, s, item_id, job_id) = foreign_claim_harness();
    kill_job(&s, &job_id);
    let released: serde_json::Value = serde_json::from_str(
        &crate::claims::with_owner_override(RESCUER, || {
            s.item(Parameters(force_req(
                "release",
                &item_id,
                Some("job vanished"),
            )))
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(released["released"], true, "{released}");
    assert!(
        released["forced_override"]
            .as_str()
            .unwrap()
            .contains(&job_id)
    );
    assert_eq!(holder_of(&tmp, &item_id), None);
    let audit = comments(&tmp, &item_id);
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert!(audit[0].starts_with(FORCE_OVERRIDE_MARKER));
    assert!(audit[0].contains("job vanished"), "{}", audit[0]);
    assert!(
        audit[0].contains(&format!("claude-code:{job_id}")),
        "{}",
        audit[0]
    );
}

#[test]
fn a_terminal_failure_comment_alone_satisfies_the_gate() {
    // Job still "queued" (not confirmed dead) — the failure comment is the
    // only evidence.
    let (tmp, s, item_id, _job_id) = foreign_claim_harness();
    post_failure(&tmp, &item_id);
    let merged: serde_json::Value = serde_json::from_str(
        &crate::claims::with_owner_override(RESCUER, || {
            s.item(Parameters(force_req(
                "check_merge",
                &item_id,
                Some("finishing #299"),
            )))
        })
        .unwrap(),
    )
    .unwrap();
    // No PR can be found in this harness, so nothing is promoted — but the
    // claim did move to the caller, with an audit trail.
    assert_eq!(merged["promoted"], false, "{merged}");
    assert!(merged["reason"].as_str().unwrap().contains("not merged"));
    assert_eq!(holder_of(&tmp, &item_id).as_deref(), Some(RESCUER));
    assert!(
        comments(&tmp, &item_id)
            .iter()
            .any(|c| c.starts_with(FORCE_OVERRIDE_MARKER) && c.contains("check_merge"))
    );
}

#[test]
fn auto_release_needs_both_a_dead_job_and_a_terminal_failure() {
    // Live job + failure comment: kept.
    let (tmp, s, item_id, job_id) = foreign_claim_harness();
    post_failure(&tmp, &item_id);
    let queue = s.job_queue_override.clone().unwrap();
    assert_eq!(auto_release_dead_claims(&s, &queue), 0);
    assert_eq!(
        holder_of(&tmp, &item_id),
        Some(format!("claude-code:{job_id}"))
    );

    // Dead job + failure comment: released, no force call, with a comment.
    kill_job(&s, &job_id);
    assert_eq!(auto_release_dead_claims(&s, &queue), 1);
    assert_eq!(holder_of(&tmp, &item_id), None);
    assert!(
        comments(&tmp, &item_id)
            .iter()
            .any(|c| c.starts_with(AUTO_RELEASE_MARKER))
    );

    // Dead job, no failure comment: kept.
    let (tmp, s, item_id, job_id) = foreign_claim_harness();
    kill_job(&s, &job_id);
    let queue = s.job_queue_override.clone().unwrap();
    assert_eq!(auto_release_dead_claims(&s, &queue), 0);
    assert_eq!(
        holder_of(&tmp, &item_id),
        Some(format!("claude-code:{job_id}"))
    );
}

#[test]
fn a_passing_pr_is_not_evidence_unless_the_caller_is_on_its_pushed_branch() {
    use crate::worktree::PrCiStatus;
    let passing = || PrCiStatus::Passing {
        number: 7,
        labels: vec![],
    };
    // A live owner's PR going green must not let a bystander take over.
    assert_eq!(branch_gate(false, passing()), None);
    assert_eq!(branch_gate(false, PrCiStatus::Merged), None);
    assert!(branch_gate(true, passing()).unwrap().contains("PR #7"));
    assert!(branch_gate(true, PrCiStatus::Merged).is_some());
    assert_eq!(branch_gate(true, PrCiStatus::Pending), None);
}

#[test]
fn caller_on_pushed_branch_requires_the_branch_and_a_pushed_head() {
    let tmp = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q", "-b", "task/1-x"]);
    git(&["commit", "-q", "--allow-empty", "-m", "one"]);
    // Not pushed yet.
    assert!(!caller_on_pushed_branch(tmp.path(), "task/1-x"));
    git(&["update-ref", "refs/remotes/origin/task/1-x", "HEAD"]);
    assert!(caller_on_pushed_branch(tmp.path(), "task/1-x"));
    // Caller on a different branch than the claim's.
    assert!(!caller_on_pushed_branch(tmp.path(), "task/2-y"));
    // Local commits ahead of what's pushed.
    git(&["commit", "-q", "--allow-empty", "-m", "two"]);
    assert!(!caller_on_pushed_branch(tmp.path(), "task/1-x"));
}

#[test]
fn forced_merged_promotion_of_an_unclaimed_item_is_audited() {
    // image-qc #299 shape: `done` never ran, so the item isn't in_review;
    // its PR is merged (checked by the caller) and nobody holds a live claim.
    let (tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let promoted = s
        .promote_forced(
            &item_id,
            RESCUER,
            crate::claims::now(),
            "finish #299",
            false,
        )
        .unwrap();
    assert!(promoted);
    let audit = comments(&tmp, &item_id);
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert!(audit[0].starts_with(FORCE_OVERRIDE_MARKER));
    assert!(audit[0].contains("finish #299"), "{}", audit[0]);

    // Already audited by `force_takeover`: no second comment.
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Other"))).unwrap()).unwrap();
    let other = created["id"].as_str().unwrap().to_string();
    assert!(
        s.promote_forced(&other, RESCUER, crate::claims::now(), "r", true)
            .unwrap()
    );
    assert!(comments(&tmp, &other).is_empty());
}

#[test]
fn forced_merged_promotion_refuses_when_someone_else_holds_the_claim() {
    let (tmp, s, item_id, job_id) = foreign_claim_harness();
    let err = s
        .promote_forced(&item_id, RESCUER, crate::claims::now(), "race", true)
        .unwrap_err();
    assert!(err.message.contains("was claimed by"), "{err:?}");
    assert_eq!(
        holder_of(&tmp, &item_id),
        Some(format!("claude-code:{job_id}"))
    );
    assert!(comments(&tmp, &item_id).is_empty());
}

#[test]
fn force_done_on_a_dead_jobs_claim_moves_the_claim_with_an_audit() {
    let (tmp, s, item_id, job_id) = foreign_claim_harness();
    kill_job(&s, &job_id);
    let result = crate::claims::with_owner_override(RESCUER, || {
        s.item(Parameters(force_req(
            "done",
            &item_id,
            Some("job vanished"),
        )))
    });
    // No worktree/remote in this harness, so `done` itself is a no-op --
    // but the override (and its audit) happened first, as documented.
    let result: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
    assert!(
        result["forced_override"]
            .as_str()
            .unwrap()
            .contains(&job_id),
        "{result}"
    );
    assert!(
        comments(&tmp, &item_id)
            .iter()
            .any(|c| c.starts_with(FORCE_OVERRIDE_MARKER) && c.contains("`done`")),
        "{:?}",
        comments(&tmp, &item_id)
    );
    assert_ne!(
        holder_of(&tmp, &item_id),
        Some(format!("claude-code:{job_id}"))
    );
}
