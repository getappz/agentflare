//! Ownership of GitHub's native auto-merge on an item's PR. The sweep arms
//! auto-merge (`merge::merge_approved_pr`) only once the approval label is
//! on and CodeRabbit findings are clear, and GitHub then merges whatever the
//! PR head is when its requirements pass -- including commits pushed after
//! the sweep judged it, since a push from a write-access account (every
//! agentflare agent) keeps auto-merge armed. So the sweep records what it
//! armed (`metadata.pr.auto_merge`: the head it judged and the PR's node
//! id) and, on every later tick, disarms its own auto-merge the moment the
//! head moves, CI fails, a conflict appears, findings come back or the label
//! is removed. Auto-merge that a human armed carries no record and is never
//! touched. Split out of `merge.rs` to keep it under the LOC gate.

use super::*;

/// The auto-merge this sweep armed on an item's PR, as recorded in
/// `metadata.pr.auto_merge`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArmedAutoMerge {
    /// The head the arming was pinned to (`expectedHeadOid`); empty when
    /// GitHub reported none, in which case head moves can't be judged.
    pub head: String,
    /// The PR's GraphQL node id, what the disarm mutation addresses.
    pub node_id: String,
}

/// The auto-merge this sweep armed on `item`'s PR, if any. `None` for a PR
/// whose auto-merge nobody here armed -- a human's, or none at all.
pub(super) fn armed_auto_merge(item: &agentflare_backend::item::Item) -> Option<ArmedAutoMerge> {
    let meta = serde_json::from_str::<serde_json::Value>(&item.metadata).ok()?;
    let armed = meta.get("pr")?.get("auto_merge")?;
    Some(ArmedAutoMerge {
        head: armed["head"].as_str().unwrap_or_default().to_string(),
        node_id: armed["node_id"].as_str()?.to_string(),
    })
}

/// Records that this sweep just armed auto-merge on `item`'s PR at `head`.
pub(super) fn record_armed_auto_merge(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    head: Option<&str>,
    node_id: &str,
) {
    set_pr_auto_merge(
        mcp,
        item,
        Some(serde_json::json!({ "head": head.unwrap_or_default(), "node_id": node_id })),
    );
}

/// Forgets a recorded arming -- after disarming it, or once GitHub reports
/// it gone.
pub(super) fn clear_armed_auto_merge(mcp: &AgentflareMcp, item: &agentflare_backend::item::Item) {
    set_pr_auto_merge(mcp, item, None);
}

/// Writes or removes `metadata.pr.auto_merge`, merged into the existing `pr`
/// record rather than replacing it. Best-effort like `persist_pr_identity`:
/// a db hiccup costs one redundant arm or disarm on a later tick.
fn set_pr_auto_merge(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    value: Option<serde_json::Value>,
) {
    let outcome = mcp.with_backend_db(|conn| {
        crate::mcp_server::merge_item_metadata(conn, &item.id, |merged| {
            let mut pr = merged
                .get("pr")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            match value {
                Some(v) => {
                    pr.insert("auto_merge".into(), v);
                }
                None => {
                    pr.remove("auto_merge");
                }
            }
            merged.insert("pr".into(), serde_json::Value::Object(pr));
        })
    });
    match outcome {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => eprintln!(
            "agentflare-supervisor: could not record auto-merge state for item #{}: {e}",
            item.sequence_id
        ),
        Err(e) => eprintln!(
            "agentflare-supervisor: could not record auto-merge state for item #{}: {e:?}",
            item.sequence_id
        ),
    }
}

/// Disarms auto-merge on PR `number` if this sweep armed it (and forgets
/// the arming); a no-op, with no network call, for auto-merge nobody here
/// armed. Returns whether it acted.
pub(super) fn disarm_our_auto_merge(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
    number: u64,
    why: &str,
) -> bool {
    let Some(armed) = armed_auto_merge(item) else {
        return false;
    };
    let Some(repo) = crate::github::RepoId::resolve_from_remote(repo_root) else {
        return false;
    };
    let Ok(client) = crate::github::Client::new() else {
        return false;
    };
    disarm_auto_merge_with(&client, &repo, number, &armed.node_id, why);
    clear_armed_auto_merge(mcp, item);
    true
}

/// `disarm_our_auto_merge`'s GitHub half, split out for mock-server tests
/// the same way `merge_approved_pr` is. Soft-fails: a disarm GitHub refused
/// is logged, and the recorded arming is still cleared by the caller so the
/// next tick re-judges from GitHub's own `autoMergeRequest`.
pub(super) fn disarm_auto_merge_with(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    number: u64,
    node_id: &str,
    why: &str,
) {
    match crate::github::graphql::disable_auto_merge(client, node_id) {
        Ok(()) => eprintln!(
            "agentflare-supervisor: disarmed GitHub auto-merge on PR #{number} in {repo}: {why}"
        ),
        Err(e) => eprintln!(
            "agentflare-supervisor: could not disarm GitHub auto-merge on PR #{number} in \
             {repo}: {}",
            e.log_safe()
        ),
    }
}

/// What a sweep tick does about a recorded arming, given the PR's fresh
/// snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ArmedVerdict {
    /// Still the head it was armed on, still armed: nothing to do.
    Keep,
    /// GitHub no longer has it armed (merged, closed, or disarmed by
    /// someone): drop the stale record.
    Forget,
    /// The head moved past the commit it was armed on: those commits were
    /// never judged for findings, so GitHub must not merge them on this
    /// arming.
    Disarm,
}

/// Judges a recorded arming against the batch snapshot. No snapshot (the
/// query for that chunk failed) keeps the record for the next tick.
pub(super) fn judge_armed_auto_merge(
    armed: &ArmedAutoMerge,
    data: Option<&crate::github::graphql::BatchPrData>,
) -> ArmedVerdict {
    let Some(data) = data else {
        return ArmedVerdict::Keep;
    };
    if data.merged || data.closed || !data.auto_merge_enabled {
        return ArmedVerdict::Forget;
    }
    match data.head_sha.as_deref() {
        Some(head) if !armed.head.is_empty() && head != armed.head => ArmedVerdict::Disarm,
        _ => ArmedVerdict::Keep,
    }
}

/// Runs before `handle_pr_status` for every item with a recorded arming:
/// disarms it when the head moved, forgets it when GitHub no longer has it.
/// Runs on every status, not just the CI-green ones, so a push that lands
/// while CI is still pending is caught before those checks can go green.
pub(super) fn reconcile_armed_auto_merge(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
    number: u64,
    data: Option<&crate::github::graphql::BatchPrData>,
) {
    let Some(armed) = armed_auto_merge(item) else {
        return;
    };
    match judge_armed_auto_merge(&armed, data) {
        ArmedVerdict::Keep => {}
        ArmedVerdict::Forget => clear_armed_auto_merge(mcp, item),
        ArmedVerdict::Disarm => {
            disarm_our_auto_merge(
                mcp,
                item,
                repo_root,
                number,
                "the PR head moved past the commit it was armed on; the new commits have not \
                 been judged yet",
            );
        }
    }
}
