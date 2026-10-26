use agentflare_backend::item::Item;

use crate::github::{models::PullRequest, pulls};

pub(super) fn flag_discovery_mismatch(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    pr: &PullRequest,
    reason: &str,
) -> Result<(), crate::github::GitHubError> {
    let label = "agentflare:needs-human";
    if !pr.labels.iter().any(|l| l.name == label) {
        crate::github::issues::add_labels(client, repo, pr.number, &[label.into()])?;
    }
    let body = format!("<!-- agentflare-review-identity-mismatch -->\n\n{reason}");
    let comments = crate::github::issues::list_comments(client, repo, pr.number, None)?;
    if !comments.iter().any(|c| c.body == body) {
        crate::github::issues::comment(client, repo, pr.number, &body)?;
    }
    Ok(())
}

/// Autonomous review needs task identity, not just a workstation-local sequence number.
pub(crate) fn pr_item_mismatch(item: &Item, pr: &PullRequest) -> Option<String> {
    pr_item_mismatch_for_instance(
        item,
        pr,
        &crate::github::bridge::config::stable_instance_id(),
    )
}

pub(super) fn pr_item_mismatch_for_instance(
    item: &Item,
    pr: &PullRequest,
    instance: &str,
) -> Option<String> {
    let body = pr.body.as_deref();
    let branch = pr.head.as_ref().map(|h| h.git_ref.as_str()).unwrap_or("");
    let meta: serde_json::Value = serde_json::from_str(&item.metadata).unwrap_or_default();
    let adopted =
        (meta["pr"]["number"].as_u64() == Some(pr.number)).then(|| &meta["pr"]["adopted_identity"]);
    let expected_id = adopted.and_then(|v| v["item"].as_str()).unwrap_or(&item.id);
    let expected_seq = if expected_id == item.id {
        Some(item.sequence_id)
    } else {
        adopted.and_then(|v| v["seq"].as_i64())
    };
    let reason = if super::pr_number_from_metadata(item).is_some_and(|n| n != pr.number) {
        Some("item already tracks a different PR")
    } else if !pulls::origin_allows(body, instance) || !pulls::tag_allows(body, expected_id) {
        Some("PR belongs to another item or instance")
    } else if let Some(origin) = pulls::origin_of(body) {
        (origin.item != expected_id || expected_seq.is_some_and(|seq| origin.seq != seq))
            .then_some("PR origin names a different item")
    } else if (meta["pr"]["number"].as_u64() == Some(pr.number)
        && meta["pr"]["identity_validated"].as_bool() == Some(true))
        || body.is_some_and(|b| b.contains(&pulls::item_id_tag(expected_id)))
        || (branch == flare_git_core::worktree::task_branch_name(item)
            && branch != format!("task/{}", item.sequence_id))
        || (super::item_owns_branch(item, branch) && title_key(&pr.title) == title_key(&item.name))
    {
        None
    } else {
        Some("PR branch and title do not identify this task")
    };
    reason.map(|r| format!(
        "PR #{} ({branch}, {:?}) cannot be repaired as item #{} ({:?}): {r}. Verify the PR-to-item mapping.",
        pr.number, pr.title, item.sequence_id, item.name
    ))
}

/// Explicit adoption keeps the source UUID while moving ownership to this instance.
pub(crate) fn record_adopted_identity(
    meta: &mut serde_json::Map<String, serde_json::Value>,
    pr: &PullRequest,
) {
    let origin = pulls::origin_of(pr.body.as_deref());
    let id = origin
        .as_ref()
        .map(|o| o.item.as_str())
        .or_else(|| pulls::item_id_of(pr.body.as_deref()));
    if let Some(record) = meta.get_mut("pr").and_then(|v| v.as_object_mut())
        && record.get("number").and_then(|v| v.as_u64()) == Some(pr.number)
    {
        record.insert("identity_validated".into(), true.into());
        if let Some(id) = id {
            record.insert(
                "adopted_identity".into(),
                serde_json::json!({"item": id, "seq": origin.as_ref().map(|o| o.seq)}),
            );
        }
    }
}

fn title_key(title: &str) -> String {
    let title = title.trim();
    let title = title
        .rsplit_once(" (#")
        .filter(|(_, suffix)| {
            suffix
                .strip_suffix(')')
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .map_or(title, |(title, _)| title);
    let title = title
        .split_once(':')
        .filter(|(prefix, _)| {
            let kind = prefix
                .split('(')
                .next()
                .unwrap_or(prefix)
                .trim_end_matches('!');
            matches!(
                kind,
                "feat"
                    | "fix"
                    | "docs"
                    | "style"
                    | "refactor"
                    | "perf"
                    | "test"
                    | "build"
                    | "ci"
                    | "chore"
                    | "revert"
            )
        })
        .map_or(title, |(_, title)| title);
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
