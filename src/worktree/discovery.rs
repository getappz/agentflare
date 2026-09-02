//! `run_review_sweep`'s extension for PRs opened outside the `item done`
//! flow -- split out of `mod.rs` purely to keep that file under the repo's
//! line-count gate; there is no dependency boundary here beyond
//! `super::pr_number_from_metadata`.

/// The set of PR numbers already tracked by *any* item in a project (not
/// just `in_review` ones) via `metadata.pr.number` -- `run_review_sweep`
/// diffs this against a repo's open PRs to find ones `discover_untracked_prs`
/// still needs to create an item for.
pub(crate) fn tracked_pr_numbers(
    items: &[agentflare_backend::item::Item],
) -> std::collections::HashSet<u64> {
    items
        .iter()
        .filter_map(super::pr_number_from_metadata)
        .collect()
}

/// The lowest-numbered (earliest) comment carrying a valid `Claim` marker,
/// paired with its owner -- comment ids are monotonic, so this is the same
/// tie-break `github::bridge`'s own issue-claim race resolves on.
fn earliest_claim_owner(comments: &[(u64, String)]) -> Option<(u64, String)> {
    comments
        .iter()
        .filter_map(|(id, body)| {
            let marker = crate::github::bridge::marker::Marker::parse(body)?;
            (marker.action == crate::github::bridge::marker::Action::Claim)
                .then_some((*id, marker.owner))
        })
        .min_by_key(|(id, _)| *id)
}

fn list_claim_comments(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    pr_number: u64,
) -> Result<Vec<(u64, String)>, crate::github::GitHubError> {
    Ok(
        crate::github::issues::list_comments(client, repo, pr_number, None)?
            .into_iter()
            .map(|c| (c.id, c.body))
            .collect(),
    )
}

/// Optimistic two-step claim on a PR, the same shape `github::bridge::tick`'s
/// `try_claim` uses for issues, minus the TTL/liveness machinery that only
/// makes sense for an actively-worked issue claim: a PR's tracking item, once
/// created, never needs to expire and be re-claimed, so "first successful
/// claim wins, forever" is the whole protocol. Multiple workstations can
/// independently discover the same PR (each has its own local item DB, none
/// synced) and would otherwise each create their own duplicate tracking
/// item; this marker comment is the one thing both can see.
///
/// Read comments; if a `Claim` marker already exists, someone (possibly this
/// same workstation, on an earlier run) already claimed it -- reject before
/// posting anything. Otherwise post our own marker, re-read, and only report
/// success if ours is now the earliest -- closing the window where another
/// workstation's claim raced in between the two reads.
pub(crate) fn claim_pr_for_discovery(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    pr_number: u64,
    owner: &str,
) -> bool {
    let before = match list_claim_comments(client, repo, pr_number) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("worktree: could not read PR #{pr_number} comments to claim it: {e}");
            return false;
        }
    };
    if earliest_claim_owner(&before).is_some() {
        return false;
    }
    let marker = crate::github::bridge::marker::Marker {
        action: crate::github::bridge::marker::Action::Claim,
        owner: owner.to_string(),
        item: "pr-discovery".to_string(),
        ts: chrono::Utc::now().timestamp(),
        hash: String::new(),
    };
    if let Err(e) = crate::github::issues::comment(
        client,
        repo,
        pr_number,
        &format!(
            "Tracking this PR for automated review (`{owner}`).\n\n{}",
            marker.render()
        ),
    ) {
        eprintln!("worktree: could not post claim marker on PR #{pr_number}: {e}");
        return false;
    }
    let after = match list_claim_comments(client, repo, pr_number) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("worktree: could not verify claim on PR #{pr_number}: {e}");
            return false;
        }
    };
    matches!(earliest_claim_owner(&after), Some((_, o)) if o == owner)
}

/// Creates an `in_review` item for every open, non-draft PR in `repo` not
/// already in `known_pr_numbers` -- PRs opened outside the `item done` flow
/// (by hand, or by an agent working ad hoc) would otherwise sit invisible to
/// `run_review_sweep` forever, even once a human adds the approval label,
/// since the sweep only ever iterates items it already knows about. Gated on
/// `github::is_trusted_author_association` the same way issue intake is
/// (`github::bridge::tick::is_trusted_author`): only the repo's own
/// owner/member/collaborator PRs get auto-tracked -- an external
/// contributor's PR must go through a human before it enters this pipeline
/// at all. Also gated on `claim_pr_for_discovery` (`owner` identifies this
/// workstation): multiple workstations independently poll the same repo with
/// no shared item DB, so without a durable marker on the PR itself, two of
/// them could both create their own duplicate tracking item for it. The
/// synthesized item's `metadata.pr` shape matches
/// `merge_and_persist_pr_identity` exactly, so every downstream sweep step
/// (CI check, self-repair, branch update, merge) treats it identically to a
/// normal item. Returns the number of items created; soft-fails to 0 on any
/// GitHub error, same as this file's other PR-lookup functions.
pub(crate) fn discover_untracked_prs(
    conn: &rusqlite::Connection,
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    project_id: &str,
    in_review_state_id: &str,
    known_pr_numbers: &std::collections::HashSet<u64>,
    owner: &str,
) -> usize {
    let prs = match crate::github::pulls::list(client, repo, "open") {
        Ok(prs) => prs,
        Err(e) => {
            eprintln!("worktree: could not list open PRs for {repo}: {e}");
            return 0;
        }
    };
    let mut created = 0;
    for pr in prs {
        if pr.draft
            || known_pr_numbers.contains(&pr.number)
            || !crate::github::is_trusted_author_association(&pr.author_association)
        {
            continue;
        }
        let Some(branch) = pr.head.as_ref().map(|h| h.git_ref.clone()) else {
            continue;
        };
        if !claim_pr_for_discovery(client, repo, pr.number, owner) {
            continue;
        }
        let description = pr
            .body
            .clone()
            .unwrap_or_else(|| format!("Auto-tracked PR: {}", pr.html_url));
        let metadata = serde_json::json!({"pr": {"number": pr.number, "branch": branch}});
        let input = agentflare_backend::item::CreateItem {
            project_id: project_id.to_string(),
            state_id: in_review_state_id.to_string(),
            name: pr.title,
            description: Some(description),
            priority: None,
            parent_id: None,
            assignee_agent: None,
            sort_order: None,
            external_source: None,
            external_id: None,
            metadata: Some(metadata.to_string()),
            label_ids: vec![],
            assignee_ids: vec![],
            dependency_ids: vec![],
            start_date: None,
            due_date: None,
        };
        match agentflare_backend::item::create(conn, input) {
            Ok(_) => created += 1,
            Err(e) => eprintln!("worktree: could not create item for PR #{}: {e}", pr.number),
        }
    }
    created
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_with_metadata(sequence_id: i64, metadata: &str) -> agentflare_backend::item::Item {
        agentflare_backend::item::Item {
            id: format!("item-{sequence_id}"),
            project_id: "p".into(),
            state_id: "s".into(),
            name: "n".into(),
            description: String::new(),
            priority: "none".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id,
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

    fn test_project_with_in_review_state(conn: &rusqlite::Connection) -> (String, String) {
        let ws = agentflare_backend::workspace::create(
            conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Test".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = agentflare_backend::project::create(
            conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id.clone(),
                name: "Test".into(),
                identifier: "T".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let in_review = agentflare_backend::state::list_by_project(conn, &proj.id)
            .unwrap()
            .into_iter()
            .find(|s| s.group_name == "in_review")
            .unwrap();
        (proj.id, in_review.id)
    }

    #[test]
    fn tracked_pr_numbers_collects_from_items_with_pr_metadata() {
        let items = vec![
            item_with_metadata(1, r#"{"pr":{"number":10,"branch":"a"}}"#),
            item_with_metadata(2, r#"{"pr":{"number":20,"branch":"b"}}"#),
            item_with_metadata(3, "{}"),
        ];

        let tracked = tracked_pr_numbers(&items);

        assert_eq!(tracked, [10, 20].into_iter().collect());
    }

    #[test]
    fn claim_pr_for_discovery_wins_when_no_existing_claim() {
        let ours = crate::github::bridge::marker::Marker {
            action: crate::github::bridge::marker::Action::Claim,
            owner: "flared:box-a".into(),
            item: "pr-discovery".into(),
            ts: 1,
            hash: String::new(),
        };
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(200, "[]"),
            crate::github::test_support::MockResponse::json(201, r#"{"id":100}"#),
            crate::github::test_support::MockResponse::json(
                200,
                &format!(
                    r#"[{{"id":100,"user":{{"login":"bot"}},"body":"{}"}}]"#,
                    ours.render()
                ),
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };

        assert!(claim_pr_for_discovery(&client, &repo, 42, "flared:box-a"));
        assert_eq!(server.requests().len(), 3);
    }

    #[test]
    fn claim_pr_for_discovery_loses_when_a_claim_already_exists() {
        let existing = crate::github::bridge::marker::Marker {
            action: crate::github::bridge::marker::Action::Claim,
            owner: "flared:box-b".into(),
            item: "pr-discovery".into(),
            ts: 1,
            hash: String::new(),
        };
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                &format!(
                    r#"[{{"id":10,"user":{{"login":"bot"}},"body":"{}"}}]"#,
                    existing.render()
                ),
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };

        assert!(!claim_pr_for_discovery(&client, &repo, 42, "flared:box-a"));
        // Reject before doing anything else -- no comment posted, no
        // re-read -- once the PR is already claimed.
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn claim_pr_for_discovery_loses_a_race_to_a_lower_comment_id() {
        let rival = crate::github::bridge::marker::Marker {
            action: crate::github::bridge::marker::Action::Claim,
            owner: "flared:box-b".into(),
            item: "pr-discovery".into(),
            ts: 1,
            hash: String::new(),
        };
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(200, "[]"),
            crate::github::test_support::MockResponse::json(201, r#"{"id":101}"#),
            crate::github::test_support::MockResponse::json(
                200,
                &format!(
                    r#"[{{"id":50,"user":{{"login":"bot"}},"body":"{}"}},{{"id":101,"user":{{"login":"bot"}},"body":"ours"}}]"#,
                    rival.render()
                ),
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };

        assert!(!claim_pr_for_discovery(&client, &repo, 42, "flared:box-a"));
        assert_eq!(server.requests().len(), 3);
    }

    #[test]
    fn discover_untracked_prs_creates_an_item_for_an_untracked_open_pr() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let ours = crate::github::bridge::marker::Marker {
            action: crate::github::bridge::marker::Action::Claim,
            owner: "flared:box-a".into(),
            item: "pr-discovery".into(),
            ts: 1,
            hash: String::new(),
        };
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":42,"html_url":"u","state":"open","title":"Fix thing","body":"does the fix","head":{"ref":"fix/thing","sha":"abc"},"author_association":"OWNER"}]"#,
            ),
            crate::github::test_support::MockResponse::json(200, "[]"),
            crate::github::test_support::MockResponse::json(201, r#"{"id":100}"#),
            crate::github::test_support::MockResponse::json(
                200,
                &format!(
                    r#"[{{"id":100,"user":{{"login":"bot"}},"body":"{}"}}]"#,
                    ours.render()
                ),
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let known = std::collections::HashSet::new();

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &known,
            "flared:box-a",
        );

        assert_eq!(created, 1);
        let items = agentflare_backend::item::list_by_project(&conn, &project_id).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Fix thing");
        assert_eq!(items[0].state_id, in_review_state_id);
        assert_eq!(items[0].description, "does the fix");
        let metadata: serde_json::Value = serde_json::from_str(&items[0].metadata).unwrap();
        assert_eq!(metadata["pr"]["number"], 42);
        assert_eq!(metadata["pr"]["branch"], "fix/thing");
    }

    #[test]
    fn discover_untracked_prs_skips_a_pr_already_tracked_by_an_item() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":42,"html_url":"u","state":"open","title":"Fix thing","head":{"ref":"fix/thing","sha":"abc"},"author_association":"OWNER"}]"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let known: std::collections::HashSet<u64> = [42].into_iter().collect();

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &known,
            "flared:box-a",
        );

        assert_eq!(created, 0);
        assert!(
            agentflare_backend::item::list_by_project(&conn, &project_id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn discover_untracked_prs_skips_a_pr_from_an_untrusted_author() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":42,"html_url":"u","state":"open","title":"Fix thing","head":{"ref":"fix/thing","sha":"abc"},"author_association":"CONTRIBUTOR"}]"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let known = std::collections::HashSet::new();

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &known,
            "flared:box-a",
        );

        assert_eq!(created, 0);
        assert!(
            agentflare_backend::item::list_by_project(&conn, &project_id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn discover_untracked_prs_skips_draft_prs() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":42,"html_url":"u","state":"open","title":"WIP","draft":true,"head":{"ref":"wip","sha":"abc"},"author_association":"OWNER"}]"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let known = std::collections::HashSet::new();

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &known,
            "flared:box-a",
        );

        assert_eq!(created, 0);
    }

    #[test]
    fn discover_untracked_prs_skips_a_pr_whose_claim_is_already_held() {
        let rival = crate::github::bridge::marker::Marker {
            action: crate::github::bridge::marker::Action::Claim,
            owner: "flared:box-b".into(),
            item: "pr-discovery".into(),
            ts: 1,
            hash: String::new(),
        };
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":42,"html_url":"u","state":"open","title":"Fix thing","head":{"ref":"fix/thing","sha":"abc"},"author_association":"OWNER"}]"#,
            ),
            crate::github::test_support::MockResponse::json(
                200,
                &format!(
                    r#"[{{"id":10,"user":{{"login":"bot"}},"body":"{}"}}]"#,
                    rival.render()
                ),
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let known = std::collections::HashSet::new();

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &known,
            "flared:box-a",
        );

        assert_eq!(created, 0);
        assert!(
            agentflare_backend::item::list_by_project(&conn, &project_id)
                .unwrap()
                .is_empty()
        );
    }
}
