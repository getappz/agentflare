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

/// The item whose own branch `branch` is: `task/<sequence_id>[-<slug>]` for
/// an item in `items` *with a local branch of that name* (sequence numbers
/// are per instance, so the name alone could be another workstation's), or
/// the branch a previous PR was recorded against in `metadata.pr.branch`.
fn item_owning_branch<'a>(
    items: &'a [agentflare_backend::item::Item],
    branch: &str,
    local_branch: &dyn Fn(&str) -> bool,
) -> Option<&'a agentflare_backend::item::Item> {
    items
        .iter()
        .find(|item| super::item_owns_branch_here(item, branch, local_branch))
}

/// Records `number`/`branch` as `item`'s PR when it has none yet, so the next
/// sweep tracks it by number like any PR opened through `item done`.
fn attach_pr_to_item(
    conn: &rusqlite::Connection,
    item: &agentflare_backend::item::Item,
    number: u64,
    branch: &str,
) {
    if super::pr_number_from_metadata(item).is_some() {
        return;
    }
    let result = crate::mcp_server::merge_item_metadata(conn, &item.id, |map| {
        map.insert(
            "pr".into(),
            serde_json::json!({"number": number, "branch": branch}),
        );
    });
    if let Err(e) = result {
        eprintln!(
            "worktree: could not attach PR #{number} to item {}: {e}",
            item.sequence_id
        );
    }
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
/// Read comments; if another workstation's `Claim` marker is already the
/// earliest, it owns the PR -- reject before posting anything. If the
/// earliest marker is *ours*, this workstation already won the claim on an
/// earlier run: `discover_untracked_prs` only asks about PRs no local item
/// tracks, so reaching here means that run's item creation never landed
/// (it runs after the claim, and a failure there is only logged). Report
/// success so the item gets created now -- rejecting our own marker used to
/// leave the PR untracked forever. Otherwise post our own marker, re-read,
/// and only report success if ours is now the earliest -- closing the window
/// where another workstation's claim raced in between the two reads.
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
    if let Some((_, existing_owner)) = earliest_claim_owner(&before) {
        return existing_owner == owner;
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

/// True when `created_at` (the PR's GitHub `created_at`) is less than
/// `grace_secs` old as of `now` -- i.e. the PR is still inside the discovery
/// grace window and must be left alone for its opener to claim. A missing or
/// unparseable timestamp is `false`: it can't prove freshness, so the caller
/// falls back to today's first-claim-wins instead of deferring on evidence
/// that will never arrive. A future timestamp (clock skew) counts as fresh.
fn within_discovery_grace(
    created_at: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
    grace_secs: u64,
) -> bool {
    let Some(created) = created_at else {
        return false;
    };
    let Ok(created) = chrono::DateTime::parse_from_rfc3339(created) else {
        return false;
    };
    let age_secs = now.signed_duration_since(created.with_timezone(&chrono::Utc));
    age_secs < chrono::Duration::seconds(i64::try_from(grace_secs).unwrap_or(i64::MAX))
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
/// them could both create their own duplicate tracking item for it.
///
/// Also skips any PR whose body already carries agentflare's own stamp --
/// either the hidden origin stamp or the legacy `pulls::opened_by_agentflare`
/// marker -- *before* the claim-comment check.
/// `known_pr_numbers` only reflects items in *this* workstation's own local
/// database, so a PR another workstation's `push_and_open_pr` just opened
/// for its own item is invisible here even though it's already tracked
/// there. The stamp lands in the PR body the instant it's created, ahead of
/// any claim comment (`push_and_open_pr` never posts one), so relying on
/// `claim_pr_for_discovery` alone left a race window: a sweep here could
/// still see the PR as unclaimed and win the (uncontested) claim, adopting a
/// second, duplicate local item and stacking a second `beacon:` label on top
/// of the real opener's (item #261, live: PR #688 ended up carrying two
/// different workstations' `beacon:` labels 23 seconds later).
///
/// A PR stamped by a *foreign* instance is never adopted here, full stop:
/// sequence numbers are per instance, so its `seq` says nothing about local
/// items (item #347 phase 2). An *unstamped* PR with no local branch evidence
/// (no `refs/heads/<branch>` here) may be another instance's fresh work that
/// simply hasn't been stamped or claimed yet, so it waits out the discovery
/// grace window (`[bridge] discovery_grace_secs`, default 600 s, measured
/// from the PR's `created_at`) before falling back to first-claim-wins.
/// Local branch evidence adopts at once -- via the owning-item attach path
/// for `task/<seq>` branches, via the claim path otherwise. A missing or
/// unparseable `created_at` can't prove freshness, so it falls back to
/// today's behavior instead of waiting on a timestamp that will never come.
/// The claim protocol itself (`claim_pr_for_discovery`, #632) is unchanged.
///
/// The synthesized item's `metadata.pr` shape matches
/// `merge_and_persist_pr_identity` exactly, so every downstream sweep step
/// (CI check, self-repair, branch update, merge) treats it identically to a
/// normal item. Returns the number of items created; soft-fails to 0 on any
/// GitHub error, same as this file's other PR-lookup functions.
#[allow(clippy::too_many_arguments)]
pub(crate) fn discover_untracked_prs(
    conn: &rusqlite::Connection,
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    project_id: &str,
    in_review_state_id: &str,
    known_pr_numbers: &std::collections::HashSet<u64>,
    owner: &str,
    local_branch: &dyn Fn(&str) -> bool,
) -> usize {
    discover_untracked_prs_with_clock(
        conn,
        client,
        repo,
        project_id,
        in_review_state_id,
        known_pr_numbers,
        owner,
        local_branch,
        chrono::Utc::now(),
        crate::github::bridge::config::discovery_grace_secs(),
    )
}

/// [`discover_untracked_prs`] with the clock and grace window injected, so
/// the freshness boundary is unit-testable without sleeping. Production
/// callers use [`discover_untracked_prs`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn discover_untracked_prs_with_clock(
    conn: &rusqlite::Connection,
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    project_id: &str,
    in_review_state_id: &str,
    known_pr_numbers: &std::collections::HashSet<u64>,
    owner: &str,
    local_branch: &dyn Fn(&str) -> bool,
    now: chrono::DateTime<chrono::Utc>,
    grace_secs: u64,
) -> usize {
    let prs = match crate::github::pulls::list(client, repo, "open") {
        Ok(prs) => prs,
        Err(e) => {
            eprintln!("worktree: could not list open PRs for {repo}: {e}");
            return 0;
        }
    };
    let mut created = 0;
    // Item #337: the items a PR's head branch can already belong to.
    let items = agentflare_backend::item::list_by_project(conn, project_id).unwrap_or_default();
    for pr in prs {
        if pr.draft
            || known_pr_numbers.contains(&pr.number)
            || !crate::github::is_trusted_author_association(&pr.author_association)
            || crate::github::pulls::opened_by_agentflare(pr.body.as_deref())
        {
            continue;
        }
        let Some(branch) = pr.head.as_ref().map(|h| h.git_ref.clone()) else {
            continue;
        };
        // An agent that opens its own PR on its item's `task/<seq>-…` branch
        // (plain `gh pr create`) never writes `metadata.pr`, so the number
        // check above calls the PR untracked. The branch names its owner:
        // attach the PR there instead of minting a second item that the
        // supervisor would then dispatch repairs against, colliding with the
        // owner's worktree (#334 beside #330).
        if let Some(owning) = item_owning_branch(&items, &branch, local_branch) {
            attach_pr_to_item(conn, owning, pr.number, &branch);
            continue;
        }
        // Item #347 phase 2: an unstamped PR with no local branch evidence
        // may be another instance's fresh work. Give its opener the grace
        // window to stamp or claim it first; only then fall back to
        // first-claim-wins. (Stamped PRs never reach here -- the
        // `opened_by_agentflare` guard above already skipped them -- and the
        // claim protocol itself is unchanged.)
        if !local_branch(&branch)
            && within_discovery_grace(pr.created_at.as_deref(), now, grace_secs)
        {
            continue;
        }
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
            Ok(_) => {
                created += 1;
                // Same starting stage label `push_and_open_pr` gives a PR
                // opened through the item-done flow -- without this, a
                // hand-opened PR discovery only ever tracks would carry no
                // agentflare lifecycle label at all, leaving a human with no
                // GitHub-visible signal that it's under the sweep's watch.
                let machine = crate::github::bridge::config::machine_label();
                if let Err(e) = crate::github::issues::add_labels(
                    client,
                    repo,
                    pr.number,
                    &[
                        "agentflare:in-review".to_string(),
                        format!("beacon:{machine}"),
                    ],
                ) {
                    eprintln!(
                        "worktree: could not label discovered PR #{}: {e}",
                        pr.number
                    );
                }
            }
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
    fn claim_pr_for_discovery_resumes_its_own_earlier_claim() {
        // An earlier run of this same workstation won the claim but its item
        // creation never landed. The claim must be honored (no second marker
        // posted) so the item can be created now, not rejected forever.
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
                &format!(
                    r#"[{{"id":10,"user":{{"login":"bot"}},"body":"{}"}}]"#,
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
        assert_eq!(server.requests().len(), 1, "no second marker is posted");
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
            &|_| true,
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
            &|_| true,
        );

        assert_eq!(created, 0);
        assert!(
            agentflare_backend::item::list_by_project(&conn, &project_id)
                .unwrap()
                .is_empty()
        );
    }

    // Regression for item #261 (live incident): PR #688, opened by one
    // workstation's `push_and_open_pr` for item #259, ended up carrying a
    // SECOND workstation's `beacon:` label too -- that second workstation's
    // own local item database had no record of item #259 or PR #688 at all
    // (per-workstation DBs aren't synced), so its `known_pr_numbers` didn't
    // exclude it, and it adopted the PR into a duplicate local item. The PR
    // body already carried agentflare's own `for item #259 via agentflare.`
    // stamp from the moment it was opened -- this test simulates exactly
    // that: `known_pr_numbers` is empty (as it would be on the second,
    // unaware workstation), but the PR body itself proves it's already
    // agentflare's, so discovery must skip it before ever reaching the
    // claim-comment step (no second mock response is queued for a comment
    // post or re-read -- the test would fail with an out-of-responses panic
    // if discovery tried to claim it anyway).
    #[test]
    fn discover_untracked_prs_skips_a_pr_already_opened_by_agentflare_elsewhere() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":688,"html_url":"u","state":"open","title":"chore: fix","head":{"ref":"task/259","sha":"abc"},"author_association":"OWNER","body":"---\n_Opened by `claude-code` on **flared:51bb8de6c33b** for item #259 via agentflare._"}]"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        // Empty, as it would be on a workstation whose own local DB never
        // heard of item #259 -- the body-marker check must still catch it.
        let known = std::collections::HashSet::new();

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &known,
            "flared:c997d745ae66",
            &|_| true,
        );

        assert_eq!(created, 0);
        assert!(
            agentflare_backend::item::list_by_project(&conn, &project_id)
                .unwrap()
                .is_empty()
        );
        // Only the initial PR list call -- no claim comment, no item create.
        assert_eq!(server.requests().len(), 1);
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
            &|_| true,
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
            &|_| true,
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
            &|_| true,
        );

        assert_eq!(created, 0);
        assert!(
            agentflare_backend::item::list_by_project(&conn, &project_id)
                .unwrap()
                .is_empty()
        );
    }

    fn create_owner_item(
        conn: &rusqlite::Connection,
        project_id: &str,
        state_id: &str,
    ) -> agentflare_backend::item::Item {
        agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project_id.to_string(),
                state_id: state_id.to_string(),
                name: "Adopt mbx".to_string(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
                start_date: None,
                due_date: None,
            },
        )
        .unwrap()
    }

    // Item #337: an agent that opens its own PR with `gh pr create` on its
    // item's `task/<seq>-…` branch never writes `metadata.pr.number`, so the
    // number-only `known_pr_numbers` check called the PR untracked and
    // minted a second item for it (#334 beside #330). The branch names its
    // owner; the PR attaches to that item and no item or claim is created --
    // no responses are queued for a claim, so trying one would panic.
    #[test]
    fn discover_untracked_prs_attaches_a_pr_to_the_item_that_owns_its_branch() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let owner_item = create_owner_item(&conn, &project_id, &in_review_state_id);
        let branch = format!(
            "task/{}-adopt-mbx-as-the-shared-rust-build-cache",
            owner_item.sequence_id
        );
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                &format!(
                    r#"[{{"number":849,"html_url":"u","state":"open","title":"feat: adopt mbx","body":"x","head":{{"ref":"{branch}","sha":"abc"}},"author_association":"OWNER"}}]"#
                ),
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &std::collections::HashSet::new(),
            "flared:box-a",
            &|_| true,
        );

        assert_eq!(
            created, 0,
            "no duplicate item for a branch a live item owns"
        );
        let items = agentflare_backend::item::list_by_project(&conn, &project_id).unwrap();
        assert_eq!(items.len(), 1);
        let metadata: serde_json::Value = serde_json::from_str(&items[0].metadata).unwrap();
        assert_eq!(
            metadata["pr"]["number"], 849,
            "the PR is attached to its owner"
        );
        assert_eq!(metadata["pr"]["branch"], branch.as_str());
    }

    #[test]
    fn a_task_branch_with_no_matching_item_still_gets_a_tracking_item() {
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
                r#"[{"number":7,"html_url":"u","state":"open","title":"Other","body":"x","head":{"ref":"task/9999-some-other-box","sha":"abc"},"author_association":"OWNER"}]"#,
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

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &std::collections::HashSet::new(),
            "flared:box-a",
            &|_| true,
        );

        assert_eq!(
            created, 1,
            "another workstation's task branch is not ours to attach"
        );
    }

    // Sequence numbers are per instance: a `task/<N>-…` branch another
    // workstation pushed (no local branch of that name here) must not attach to
    // this instance's unrelated item #N. It falls through to the normal claim
    // path and is tracked as its own item.
    #[test]
    fn a_foreign_branch_matching_a_local_sequence_number_is_not_attached() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let unrelated = create_owner_item(&conn, &project_id, &in_review_state_id);
        let branch = format!("task/{}-from-another-workstation", unrelated.sequence_id);
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
                &format!(
                    r#"[{{"number":321,"html_url":"u","state":"open","title":"Other box","body":"x","head":{{"ref":"{branch}","sha":"abc"}},"author_association":"OWNER"}}]"#
                ),
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

        let created = discover_untracked_prs(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &std::collections::HashSet::new(),
            "flared:box-a",
            &|_| false,
        );

        assert_eq!(created, 1, "tracked as its own item, not attached");
        let items = agentflare_backend::item::list_by_project(&conn, &project_id).unwrap();
        let unrelated_now = items.iter().find(|i| i.id == unrelated.id).unwrap();
        assert!(
            !unrelated_now.metadata.contains("321"),
            "the unrelated local item must not receive the foreign PR: {}",
            unrelated_now.metadata
        );
    }

    // A branch the item itself recorded stays attached without local evidence
    // (e.g. its worktree was cleaned but the PR is still open).
    #[test]
    fn a_recorded_pr_branch_attaches_without_a_local_branch() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let item = create_owner_item(&conn, &project_id, &in_review_state_id);
        crate::mcp_server::merge_item_metadata(&conn, &item.id, |m| {
            m.insert(
                "pr".into(),
                serde_json::json!({"number": 1, "branch": "feature/recorded"}),
            );
        })
        .unwrap();
        let items = agentflare_backend::item::list_by_project(&conn, &project_id).unwrap();
        assert!(item_owning_branch(&items, "feature/recorded", &|_| false).is_some());
        assert!(item_owning_branch(&items, "feature/other", &|_| false).is_none());
    }

    // Item #347 phase 2 (discovery eligibility): the grace-window predicate.
    #[test]
    fn within_discovery_grace_holds_fresh_prs_and_releases_old_ones() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-07T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        // 60 s old with a 600 s window: still inside.
        assert!(within_discovery_grace(
            Some("2026-10-07T11:59:00Z"),
            now,
            600
        ));
        // 3600 s old: outside.
        assert!(!within_discovery_grace(
            Some("2026-10-07T11:00:00Z"),
            now,
            600
        ));
        // No timestamp proves nothing: fall back to first-claim-wins.
        assert!(!within_discovery_grace(None, now, 600));
        assert!(!within_discovery_grace(Some("not-a-timestamp"), now, 600));
        // Clock skew (PR "from the future") waits rather than rushing.
        assert!(within_discovery_grace(
            Some("2026-10-07T12:05:00Z"),
            now,
            600
        ));
        // A zero window never holds anything.
        assert!(!within_discovery_grace(
            Some("2026-10-07T11:59:59Z"),
            now,
            0
        ));
    }

    fn clock_at(rfc3339: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    fn unstamped_pr_json(number: u64, branch: &str, created_at: &str) -> String {
        format!(
            r#"[{{"number":{number},"html_url":"u","state":"open","title":"Hand opened","body":"hand opened, no stamp","head":{{"ref":"{branch}","sha":"abc"}},"author_association":"OWNER","created_at":"{created_at}"}}]"#
        )
    }

    fn claim_win_responses(owner: &str) -> Vec<crate::github::test_support::MockResponse> {
        let marker = crate::github::bridge::marker::Marker {
            action: crate::github::bridge::marker::Action::Claim,
            owner: owner.to_string(),
            item: "pr-discovery".to_string(),
            ts: 1,
            hash: String::new(),
        };
        vec![
            crate::github::test_support::MockResponse::json(200, "[]"),
            crate::github::test_support::MockResponse::json(201, r#"{"id":100}"#),
            crate::github::test_support::MockResponse::json(
                200,
                &format!(
                    r#"[{{"id":100,"user":{{"login":"bot"}},"body":"{}"}}]"#,
                    marker.render()
                ),
            ),
        ]
    }

    #[allow(clippy::too_many_arguments)]
    fn discover_with_clock(
        conn: &rusqlite::Connection,
        client: &crate::github::Client,
        repo: &crate::github::RepoId,
        project_id: &str,
        in_review_state_id: &str,
        local_branch: &dyn Fn(&str) -> bool,
        now: chrono::DateTime<chrono::Utc>,
        grace_secs: u64,
    ) -> usize {
        discover_untracked_prs_with_clock(
            conn,
            client,
            repo,
            project_id,
            in_review_state_id,
            &std::collections::HashSet::new(),
            "flared:box-a",
            local_branch,
            now,
            grace_secs,
        )
    }

    // Item #347 phase 2: an unstamped PR with no local branch evidence that is
    // still inside the grace window is left alone -- no claim comment is even
    // attempted (only the PR-list response is queued; anything more would
    // panic on an empty mock queue).
    #[test]
    fn discover_skips_a_fresh_unstamped_pr_without_local_evidence() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                &unstamped_pr_json(42, "fix/fresh-hand-opened", "2026-10-07T11:59:00Z"),
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };

        let created = discover_with_clock(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &|_| false,
            clock_at("2026-10-07T12:00:00Z"),
            600,
        );

        assert_eq!(created, 0);
        assert!(
            agentflare_backend::item::list_by_project(&conn, &project_id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(server.requests().len(), 1, "list only, never a claim");
    }

    // The same PR once the grace window has passed falls back to today's
    // first-claim-wins path and is adopted.
    #[test]
    fn discover_adopts_a_stale_unstamped_pr_without_local_evidence() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let mut responses = vec![crate::github::test_support::MockResponse::json(
            200,
            &unstamped_pr_json(42, "fix/stale-hand-opened", "2026-10-07T11:00:00Z"),
        )];
        responses.extend(claim_win_responses("flared:box-a"));
        let server = crate::github::test_support::MockServer::start(responses);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };

        let created = discover_with_clock(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &|_| false,
            clock_at("2026-10-07T12:00:00Z"),
            600,
        );

        assert_eq!(created, 1);
    }

    // Local branch evidence adopts at once even inside the grace window --
    // this workstation can see the branch, so the PR is (also) its own work
    // to track, not a stranger's to wait on.
    #[test]
    fn discover_adopts_at_once_with_local_branch_evidence_inside_grace() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let mut responses = vec![crate::github::test_support::MockResponse::json(
            200,
            &unstamped_pr_json(42, "fix/fresh-but-local", "2026-10-07T11:59:00Z"),
        )];
        responses.extend(claim_win_responses("flared:box-a"));
        let server = crate::github::test_support::MockServer::start(responses);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };

        let created = discover_with_clock(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &|_| true,
            clock_at("2026-10-07T12:00:00Z"),
            600,
        );

        assert_eq!(created, 1);
    }

    // Item #347 phase 2: a PR stamped by a foreign instance is never adopted
    // -- no matter how stale -- and never even reaches the claim step.
    #[test]
    fn discover_never_adopts_a_foreign_stamped_pr() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let (project_id, in_review_state_id) = test_project_with_in_review_state(&conn);
        let body = format!(
            "---\n_Opened by `a` on **m** for item #259 via agentflare._\n{}",
            crate::github::pulls::origin_tag(
                "flared:other-workstation",
                "other-uuid",
                259,
                "task/259-x"
            ),
        );
        let pr_json = serde_json::json!([{
            "number": 688, "html_url": "u", "state": "open", "title": "Other box",
            "body": body, "head": {"ref": "task/259-x", "sha": "abc"},
            "author_association": "OWNER", "created_at": "2026-10-07T11:00:00Z",
        }])
        .to_string();
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(200, &pr_json),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };

        let created = discover_with_clock(
            &conn,
            &client,
            &repo,
            &project_id,
            &in_review_state_id,
            &|_| false,
            clock_at("2026-10-07T12:00:00Z"),
            600,
        );

        assert_eq!(created, 0);
        assert!(
            agentflare_backend::item::list_by_project(&conn, &project_id)
                .unwrap()
                .is_empty()
        );
        assert_eq!(server.requests().len(), 1, "list only, never a claim");
    }
}
