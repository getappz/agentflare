//! GitHub PR issue comments when the review sweep will not auto-merge.
//! Wording is GitHub-flavored markdown (not Telegram HTML).

use crate::mcp_server::AgentflareMcp;
use agentflare_backend::item::Item;

/// Marker prefix on a one-time “why merge is blocked” PR comment.
pub(super) const PR_MERGE_BLOCKED_MARKER: &str = "## supervisor — auto-merge blocked";

/// Item-metadata key: fingerprint of the last posted `PR_MERGE_BLOCKED_MARKER` body.
pub(super) const PR_MERGE_BLOCK_NOTIFIED_KEY: &str = "pr_merge_block_notified_for";

/// Builds a consistent PR comment: short title marker, why, checklist.
pub(super) fn format_merge_block_comment(why: &str, actions: &[&str]) -> String {
    let mut body = format!("{PR_MERGE_BLOCKED_MARKER}\n\n");
    body.push_str("### Why\n\n");
    body.push_str(why.trim());
    body.push_str("\n\n### What you need to do\n\n");
    for action in actions {
        body.push_str("- [ ] ");
        body.push_str(action.trim());
        body.push('\n');
    }
    body
}

fn current_metadata(
    mcp: &AgentflareMcp,
    item: &Item,
) -> serde_json::Map<String, serde_json::Value> {
    let fresh = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item.id))
        .ok()
        .and_then(Result::ok)
        .map(|i| i.metadata)
        .unwrap_or_else(|| item.metadata.clone());
    crate::mcp_server::metadata_object(&fresh)
}

fn update_metadata(
    mcp: &AgentflareMcp,
    item_id: &str,
    merge: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> bool {
    mcp.with_backend_db(|conn| crate::mcp_server::merge_item_metadata(conn, item_id, merge))
        .ok()
        .and_then(Result::ok)
        .is_some()
}

fn already_notified(mcp: &AgentflareMcp, item: &Item, fingerprint: &str) -> bool {
    current_metadata(mcp, item)
        .get(PR_MERGE_BLOCK_NOTIFIED_KEY)
        .and_then(|v| v.as_str())
        == Some(fingerprint)
}

/// Posts `body` on the PR once per `fingerprint` (stored on the item). Never
/// errors outward — same fail-open contract as `update_pr_stage`.
pub(super) fn post_pr_merge_block_once(
    mcp: &AgentflareMcp,
    item: &Item,
    folder_path: &str,
    pr_number: u64,
    fingerprint: &str,
    body: &str,
) {
    if body.is_empty() || already_notified(mcp, item, fingerprint) {
        return;
    }
    #[cfg(test)]
    super::record_pr_stage_comment_for_test(body);
    let Some(repo) = crate::github::RepoId::resolve_from_remote(std::path::Path::new(folder_path))
    else {
        return;
    };
    let Ok(client) = crate::github::Client::new() else {
        return;
    };
    if let Err(e) = crate::github::issues::comment(&client, &repo, pr_number, body) {
        eprintln!(
            "agentflare-supervisor: could not comment on PR #{pr_number}: {}",
            e.log_safe()
        );
        return;
    }
    update_metadata(mcp, &item.id, |m| {
        m.insert(PR_MERGE_BLOCK_NOTIFIED_KEY.into(), fingerprint.into());
    });
}

/// PR comment when `status:pr:approved` is still missing.
pub(super) fn post_missing_approval_pr_comment(
    mcp: &AgentflareMcp,
    item: &Item,
    folder_path: &str,
    pr_number: u64,
    approval_label: &str,
) {
    let body = format_merge_block_comment(
        &format!(
            "This PR is CI-green and mergeable, but the review sweep will not merge it until a \
             human adds the `{approval_label}` label on GitHub."
        ),
        &[
            &format!("Add the `{approval_label}` label to this PR."),
            "If `agentflare:needs-human` is also on the PR, read the supervisor comments above \
             and clear the underlying blocker before expecting auto-merge.",
        ],
    );
    post_pr_merge_block_once(mcp, item, folder_path, pr_number, "approval-label", &body);
}

/// PR comment when the item carries `needs-human-gate` / `needs-manual-dispatch`.
pub(super) fn post_item_dispatch_gate_pr_comment(
    mcp: &AgentflareMcp,
    item: &Item,
    folder_path: &str,
    pr_number: u64,
) {
    let body = format_merge_block_comment(
        "This PR is CI-green and approved, but the sweep is skipping auto-merge because the \
         linked work item carries a dispatch gate label (`needs-human-gate` or \
         `needs-manual-dispatch`).",
        &[
            "Open the work item and remove the gate label once you are ready for the sweep to \
             continue (or merge this PR manually on GitHub).",
            "If the gate was set by a repair cap or review escalation, address those threads \
             first.",
        ],
    );
    post_pr_merge_block_once(
        mcp,
        item,
        folder_path,
        pr_number,
        "item-dispatch-gate",
        &body,
    );
}

/// PR comment when GitHub branch protection blocks merge pending human review.
pub(super) fn post_github_review_block_pr_comment(
    mcp: &AgentflareMcp,
    item: &Item,
    folder_path: &str,
    pr_number: u64,
    changes_requested: bool,
    label_missing: bool,
    approval_label: &str,
) {
    let why = if changes_requested {
        "GitHub is blocking the merge because a reviewer requested changes."
    } else {
        "GitHub is blocking the merge until an approving review lands (branch protection)."
    };
    let mut actions: Vec<String> = vec![
        "Read the open review feedback on the PR **Files changed** tab.".into(),
        "Push fixes or reply on review threads until GitHub shows the PR as mergeable.".into(),
    ];
    if label_missing {
        actions.push(format!(
            "Add `{approval_label}` once GitHub allows merge so the sweep can finish the merge."
        ));
    }
    let action_refs: Vec<&str> = actions.iter().map(String::as_str).collect();
    let body = format_merge_block_comment(why, &action_refs);
    post_pr_merge_block_once(
        mcp,
        item,
        folder_path,
        pr_number,
        "github-review-block",
        &body,
    );
}

/// PR comment when unresolved review-bot threads hold merge (no repair dispatch this tick).
pub(super) fn post_review_threads_hold_pr_comment(
    mcp: &AgentflareMcp,
    item: &Item,
    folder_path: &str,
    pr_number: u64,
    blocking: usize,
    escalated: usize,
    waiting_push: usize,
    unknown: bool,
) {
    let fingerprint = format!("review-hold:{blocking}:{escalated}:{waiting_push}:{unknown}");
    let why = if unknown {
        "The sweep could not read review threads on this PR (GitHub API error or missing \
         credentials), so auto-merge is held."
            .to_string()
    } else if escalated > 0 {
        format!(
            "{escalated} review thread(s) exceeded the automatic repair round limit and need a \
             human decision; {blocking} non-optional thread(s) still block merge."
        )
    } else if waiting_push > 0 {
        format!(
            "A fix was reported but its commit is not on the PR branch yet ({waiting_push} \
             thread(s) waiting on push)."
        )
    } else {
        format!(
            "{blocking} unresolved review-bot thread(s) still block merge (waiting on bot \
             acceptance or human resolution)."
        )
    };
    let actions = if unknown {
        vec![
            "Retry after GitHub/API access is restored, or merge manually if you have verified the PR.",
        ]
    } else {
        vec![
            "Resolve or reply on the open CodeRabbit (review-bot) threads on the PR.",
            "Push any pending fixes, then record outcomes with `agentflare item review_result` if an agent worked the item.",
            "Remove `agentflare:needs-human` after threads are resolved if the label was applied by the sweep.",
        ]
    };
    let body = format_merge_block_comment(&why, &actions);
    post_pr_merge_block_once(mcp, item, folder_path, pr_number, &fingerprint, &body);
}

/// Body for the review-repair cap (`CODERABBIT_REPAIR_CAP_MARKER`); also used on the PR via
/// `update_pr_stage` and idempotent `post_pr_merge_block_once`.
pub(super) fn format_repair_cap_pr_comment(
    cap_marker: &str,
    fingerprint: &str,
    findings_count: usize,
    cap: u32,
) -> String {
    let why = format!(
        "{findings_count} unresolved review-bot finding(s) remain after {cap} automatic repair \
         attempt(s) (self-repair cap)."
    );
    let actions = [
        "Resolve or dismiss the open review threads on this PR, or merge manually if you accept the risk.",
        "Remove `agentflare:needs-human` after you have handled the findings.",
        "Clear `needs-human-gate` on the work item when you want agents to resume.",
    ];
    let checklist = format_merge_block_comment(&why, &actions);
    format!("{cap_marker}\n\nfindings fingerprint: {fingerprint}\n\n{checklist}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_block_comment_renders_markdown_sections() {
        let body = format_merge_block_comment(
            "CI is green but label missing.",
            &["Add the label.", "Merge manually if needed."],
        );
        assert!(body.starts_with(PR_MERGE_BLOCKED_MARKER));
        assert!(body.contains("### Why"));
        assert!(body.contains("### What you need to do"));
        assert!(body.contains("- [ ] Add the label."));
    }
}
