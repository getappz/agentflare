//! The draft half of the PR lifecycle: `push_and_open_pr` opens every PR
//! as a draft, and this module flips it to "ready for review" once
//! `item_done` has moved the item to in_review -- and again from
//! `supervisor::run_review_sweep` if that first flip never landed, so no
//! in-review item stalls behind a draft nobody was asked to look at.
//! Split out of `mod.rs` to keep it under the repo's line-count gate.
//!
//! `metadata.pr.ready` records that agentflare itself marked the PR ready.
//! The sweep only flips a draft whose flag is missing: a draft that carries
//! it was converted back by a human, on purpose, and is left alone.

use crate::github::identity::RepoId;

/// True once agentflare has marked this item's PR ready for review
/// (`metadata.pr.ready`), so a later draft is a human's doing.
pub(crate) fn pr_marked_ready(item: &agentflare_backend::item::Item) -> bool {
    serde_json::from_str::<serde_json::Value>(&item.metadata)
        .ok()
        .is_some_and(|m| m["pr"]["ready"] == true)
}

/// Marks PR `number` ready for review and records it on the item. Soft-
/// fails like the rest of the PR path: no resolvable remote or credentials
/// just means "not now", and the sweep retries next tick. Returns whether
/// the PR is known ready afterwards.
pub(crate) fn mark_pr_ready(
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
    number: u64,
    node_id: Option<&str>,
) -> bool {
    let Some(repo) = RepoId::resolve_from_remote(repo_root) else {
        return false;
    };
    let Ok(client) = crate::github::Client::new() else {
        return false;
    };
    match mark_pr_ready_with(&client, &repo, number, node_id) {
        Ok(()) => {
            persist_pr_ready(item);
            true
        }
        Err(e) => {
            eprintln!(
                "worktree: could not mark PR #{number} in {repo} ready for review for item {}: {e}",
                item.id
            );
            false
        }
    }
}

/// `mark_pr_ready`'s GitHub half, split out for mock-server tests. Fetches
/// the node id when the caller has none (the REST list endpoint the
/// branch-heuristic path uses carries it too, but an older `metadata.pr`
/// record may not), and treats GitHub's "already ready" refusal as success.
pub(crate) fn mark_pr_ready_with(
    client: &crate::github::Client,
    repo: &RepoId,
    number: u64,
    node_id: Option<&str>,
) -> Result<(), crate::github::GitHubError> {
    let node_id = match node_id {
        Some(id) => id.to_string(),
        None => {
            let pr = crate::github::pulls::get(client, repo, number)?;
            if !pr.draft {
                return Ok(());
            }
            pr.node_id.ok_or_else(|| {
                crate::github::GitHubError::Parse(format!("PR #{number} has no node_id"))
            })?
        }
    };
    match crate::github::graphql::mark_ready_for_review(client, &node_id) {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().to_lowercase().contains("not a draft") => Ok(()),
        Err(e) => Err(e),
    }
}

/// Records `metadata.pr.ready = true` on the item, merged into the existing
/// `pr` record (number, branch) rather than replacing it. Best-effort, same
/// as `persist_pr_identity`: a db hiccup here costs at most one redundant
/// ready-for-review call from the sweep.
fn persist_pr_ready(item: &agentflare_backend::item::Item) {
    let conn = match agentflare_backend::db::open_db(&crate::vent::paths::backend_db_path()) {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!(
                "worktree: could not open backend db to record PR readiness for item {}: {e}",
                item.id
            );
            return;
        }
    };
    persist_pr_ready_in(&conn, item);
}

pub(crate) fn persist_pr_ready_in(
    conn: &rusqlite::Connection,
    item: &agentflare_backend::item::Item,
) {
    if let Err(e) = crate::mcp_server::merge_item_metadata(conn, &item.id, |merged| {
        let mut pr = merged
            .get("pr")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        pr.insert("ready".into(), serde_json::Value::Bool(true));
        merged.insert("pr".into(), serde_json::Value::Object(pr));
    }) {
        eprintln!(
            "worktree: could not record PR readiness for item {}: {e}",
            item.id
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::test_support::{MockResponse, MockServer};

    fn repo() -> RepoId {
        RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    fn item_with_metadata(metadata: &str) -> agentflare_backend::item::Item {
        agentflare_backend::item::Item {
            id: "item-7".into(),
            project_id: "p".into(),
            state_id: "s".into(),
            name: "n".into(),
            description: String::new(),
            priority: "none".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id: 7,
            sort_order: 0.0,
            started_at: None,
            completed_at: None,
            archived_at: None,
            external_source: None,
            external_id: None,
            metadata: metadata.into(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
            start_date: None,
            due_date: None,
        }
    }

    #[test]
    fn pr_marked_ready_reads_the_flag_and_defaults_false() {
        assert!(pr_marked_ready(&item_with_metadata(
            r#"{"pr":{"number":70,"branch":"task/7","ready":true}}"#
        )));
        assert!(!pr_marked_ready(&item_with_metadata(
            r#"{"pr":{"number":70,"branch":"task/7"}}"#
        )));
        assert!(!pr_marked_ready(&item_with_metadata("not json")));
    }

    #[test]
    fn mark_pr_ready_with_uses_the_given_node_id_without_a_lookup() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"markPullRequestReadyForReview":{"pullRequest":{"isDraft":false}}}}"#,
        )]);
        let client = server.client(Some("tok"));
        mark_pr_ready_with(&client, &repo(), 70, Some("PR_70")).unwrap();
        let reqs = server.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, "/graphql");
        let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
        assert_eq!(sent["variables"]["input"]["pullRequestId"], "PR_70");
    }

    #[test]
    fn mark_pr_ready_with_looks_the_node_id_up_when_missing_and_skips_a_ready_pr() {
        let server = MockServer::start(vec![
            MockResponse::json(
                200,
                r#"{"number":70,"html_url":"u","state":"open","title":"t","draft":true,"node_id":"PR_70"}"#,
            ),
            MockResponse::json(
                200,
                r#"{"data":{"markPullRequestReadyForReview":{"pullRequest":{"isDraft":false}}}}"#,
            ),
            MockResponse::json(
                200,
                r#"{"number":71,"html_url":"u","state":"open","title":"t","draft":false,"node_id":"PR_71"}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        mark_pr_ready_with(&client, &repo(), 70, None).unwrap();
        mark_pr_ready_with(&client, &repo(), 71, None).unwrap();
        let reqs = server.requests();
        assert_eq!(reqs.len(), 3, "an already-ready PR needs no mutation");
        assert_eq!(reqs[0].path, "/repos/o/r/pulls/70");
        let sent: serde_json::Value = serde_json::from_str(&reqs[1].body).unwrap();
        assert_eq!(sent["variables"]["input"]["pullRequestId"], "PR_70");
        assert_eq!(reqs[2].path, "/repos/o/r/pulls/71");
    }

    #[test]
    fn mark_pr_ready_with_treats_githubs_already_ready_refusal_as_success() {
        let server = MockServer::start(vec![MockResponse::json(
            200,
            r#"{"data":{"markPullRequestReadyForReview":null},"errors":[{"message":"Pull request is not a draft"}]}"#,
        )]);
        let client = server.client(Some("tok"));
        assert!(mark_pr_ready_with(&client, &repo(), 70, Some("PR_70")).is_ok());
    }

    #[test]
    fn mark_pr_ready_with_surfaces_other_errors() {
        let server = MockServer::start(vec![MockResponse::json(500, r#"{"message":"boom"}"#)]);
        let client = server.client(Some("tok"));
        assert!(mark_pr_ready_with(&client, &repo(), 70, Some("PR_70")).is_err());
    }
}
