    fn duplicate_pr(number: u64, merged: bool) -> crate::github::models::PullRequest {
        let merged_at = if merged {
            r#","merged_at":"2026-08-01T00:00:00Z""#
        } else {
            ""
        };
        serde_json::from_str(&format!(
            r#"{{"number":{number},"html_url":"https://github.com/o/r/pull/{number}","state":"closed","title":"t"{merged_at}}}"#
        ))
        .unwrap()
    }

    /// A genuinely open (unmerged, unclosed) PR, unlike [`duplicate_pr`]'s
    /// unmerged case, which is closed.
    fn open_duplicate_pr(number: u64) -> crate::github::models::PullRequest {
        serde_json::from_str(&format!(
            r#"{{"number":{number},"html_url":"https://github.com/o/r/pull/{number}","state":"open","title":"t"}}"#
        ))
        .unwrap()
    }

    /// A genuinely open PR whose head branch is `branch` -- for pinning the
    /// "this is my own PR, not a duplicate" exclusion (item #186's orphan
    /// bug).
    fn open_duplicate_pr_on_branch(number: u64, branch: &str) -> crate::github::models::PullRequest {
        serde_json::from_str(&format!(
            r#"{{"number":{number},"html_url":"https://github.com/o/r/pull/{number}","state":"open","title":"t","head":{{"ref":"{branch}"}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn pick_duplicate_pr_prefers_a_merged_match_over_an_open_one() {
        let picked = pick_duplicate_pr(vec![open_duplicate_pr(1), duplicate_pr(2, true)], None, None)
            .unwrap();
        assert_eq!(picked.number, 2);
    }

    #[test]
    fn pick_duplicate_pr_returns_none_for_an_empty_list() {
        assert!(pick_duplicate_pr(vec![], None, None).is_none());
    }

    /// A closed-but-unmerged PR (a previously abandoned attempt, e.g. one
    /// found superseded and closed rather than merged) must not be treated
    /// as an open duplicate — that would permanently block a legitimate
    /// redispatch. `duplicate_pr(_, false)` builds exactly this shape:
    /// `state: "closed"`, `merged_at: None`.
    #[test]
    fn pick_duplicate_pr_ignores_a_closed_unmerged_pr() {
        assert!(pick_duplicate_pr(vec![duplicate_pr(3, false)], None, None).is_none());
    }

    #[test]
    fn pick_duplicate_pr_selects_a_genuinely_open_pr_when_no_merged_match_exists() {
        let picked =
            pick_duplicate_pr(vec![duplicate_pr(4, false), open_duplicate_pr(5)], None, None)
                .unwrap();
        assert_eq!(picked.number, 5);
    }

    /// Item #186: a self-repair job reclaims its own item's existing
    /// worktree/branch, so the open PR `find_by_item_marker` turns up is
    /// this job's own PR, not a competing duplicate -- it must not be
    /// short-circuited away.
    #[test]
    fn pick_duplicate_pr_excludes_an_open_pr_already_on_the_current_branch() {
        assert!(
            pick_duplicate_pr(
                vec![open_duplicate_pr_on_branch(6, "task/186-toolchain-sniffer")],
                Some("task/186-toolchain-sniffer"),
                None,
            )
            .is_none()
        );
    }

    /// A genuinely different open PR (different branch) for the same item
    /// must still be treated as a duplicate even when a current branch is
    /// known -- the exclusion above is specific to *this* job's own PR, not
    /// a blanket bypass whenever a self-repair job happens to be running.
    #[test]
    fn pick_duplicate_pr_still_flags_an_open_pr_on_a_different_branch() {
        let picked = pick_duplicate_pr(
            vec![open_duplicate_pr_on_branch(7, "task/186-someone-elses-attempt")],
            Some("task/186-toolchain-sniffer"),
            None,
        )
        .unwrap();
        assert_eq!(picked.number, 7);
    }

    /// Item #192: `metadata.pr.number` is a more durable "this is my own PR"
    /// signal than the current branch -- it doesn't depend on the worktree
    /// actually being checked out onto that exact branch right now, which is
    /// what kept blocking item #186/#597's self-repair attempts even after
    /// the branch-based exclusion landed.
    #[test]
    fn pick_duplicate_pr_excludes_an_open_pr_matching_the_items_own_tracked_pr_number() {
        assert!(
            pick_duplicate_pr(vec![open_duplicate_pr(8)], None, Some(8)).is_none()
        );
    }

    /// The own-PR-number exclusion is specific to that exact PR -- a
    /// different open PR for the same item (someone else's competing
    /// attempt) must still be flagged even when the item has its own
    /// tracked PR recorded.
    #[test]
    fn pick_duplicate_pr_still_flags_a_different_open_pr_when_own_pr_number_does_not_match() {
        let picked = pick_duplicate_pr(vec![open_duplicate_pr(9)], None, Some(8)).unwrap();
        assert_eq!(picked.number, 9);
    }

    fn open_duplicate_pr_created_at(number: u64, created_at: &str) -> crate::github::models::PullRequest {
        serde_json::from_str(&format!(
            r#"{{"number":{number},"html_url":"https://github.com/o/r/pull/{number}","state":"open","title":"t","created_at":"{created_at}"}}"#
        ))
        .unwrap()
    }

    fn days_ago(days: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now() - chrono::Duration::days(days)
    }

    /// Item #192's core fix: a duplicate that's been CI-red long enough must
    /// stop blocking redispatch/re-review instead of shielding a bad PR
    /// forever (item #186/#597).
    #[test]
    fn is_stale_ci_red_is_true_once_a_ci_failing_pr_has_sat_open_past_the_threshold() {
        let pr = open_duplicate_pr_created_at(1, &days_ago(4).to_rfc3339());
        assert!(is_stale_ci_red(&pr, true, chrono::Utc::now()));
    }

    #[test]
    fn is_stale_ci_red_is_false_for_a_ci_failing_pr_still_within_the_threshold() {
        let pr = open_duplicate_pr_created_at(2, &days_ago(1).to_rfc3339());
        assert!(!is_stale_ci_red(&pr, true, chrono::Utc::now()));
    }

    /// An old PR whose CI isn't actually failing (passing, pending, or
    /// unknown) is still plausibly in-progress work, not a dead end -- age
    /// alone must not be enough to unblock it.
    #[test]
    fn is_stale_ci_red_is_false_for_an_old_pr_that_is_not_ci_failing() {
        let pr = open_duplicate_pr_created_at(3, &days_ago(30).to_rfc3339());
        assert!(!is_stale_ci_red(&pr, false, chrono::Utc::now()));
    }

    #[test]
    fn is_stale_ci_red_is_false_when_created_at_is_missing() {
        let pr = open_duplicate_pr(4);
        assert!(!is_stale_ci_red(&pr, true, chrono::Utc::now()));
    }

    /// Item #164's acceptance criterion: a merged duplicate PR self-heals
    /// the item to "completed" instead of letting a redispatch re-do
    /// already-merged work (the near-miss from items #122/#156).
    #[test]
    fn handle_duplicate_pr_auto_completes_on_a_merged_match() {
        crate::paths::test_support::with_temp_home(|| {
            let tmp = tempfile::tempdir().unwrap();
            let repo_root = tmp.path().join("repo");
            std::fs::create_dir_all(&repo_root).unwrap();
            init_test_repo(&repo_root);
            let mcp = AgentflareMcp::for_project_dir(repo_root.clone());
            let item = mcp.with_backend_db(|conn| seeded_item(&mcp, conn)).unwrap();
            let claim = mcp
                .item_claim(ItemRequest {
                    action: "claim".into(),
                    id: Some(item.id.clone()),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&claim).unwrap()["status"],
                "acquired"
            );

            let mut guard = ClaimGuard::new(&mcp, &item.id);
            let mut log = Vec::new();
            let outcome = handle_duplicate_pr(
                &mcp,
                &item.id,
                &item,
                &repo_root,
                &duplicate_pr(42, true),
                None,
                &mut guard,
                &mut log,
            );
            assert_eq!(outcome.exit_code, 0);

            let refreshed = mcp
                .with_backend_db(|conn| agentflare_backend::item::get(conn, &item.id))
                .unwrap()
                .unwrap();
            let state = mcp
                .with_backend_db(|conn| agentflare_backend::state::get(conn, &refreshed.state_id))
                .unwrap()
                .unwrap();
            assert_eq!(state.group_name, "completed");

            let claim_after = mcp
                .item_claim(ItemRequest {
                    action: "claim".into(),
                    id: Some(item.id.clone()),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&claim_after).unwrap()["status"],
                "acquired",
                "claim must be released so a re-claim succeeds"
            );

            let comments = mcp
                .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
                .unwrap()
                .unwrap();
            assert!(
                comments
                    .iter()
                    .any(|c| c.body.contains("duplicate work detected") && c.body.contains("#42")),
                "expected a duplicate-work comment naming the merged PR, got: {comments:?}"
            );
        });
    }

    /// The other half of item #164's MVP: an open (unmerged) duplicate skips
    /// dispatch and flags for a human instead of self-completing or opening
    /// a second PR.
    #[test]
    fn handle_duplicate_pr_flags_for_review_on_an_open_match() {
        crate::paths::test_support::with_temp_home(|| {
            let tmp = tempfile::tempdir().unwrap();
            let repo_root = tmp.path().join("repo");
            std::fs::create_dir_all(&repo_root).unwrap();
            init_test_repo(&repo_root);
            let mcp = AgentflareMcp::for_project_dir(repo_root.clone());
            let item = mcp.with_backend_db(|conn| seeded_item(&mcp, conn)).unwrap();
            let claim = mcp
                .item_claim(ItemRequest {
                    action: "claim".into(),
                    id: Some(item.id.clone()),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&claim).unwrap()["status"],
                "acquired"
            );
            // Capture state post-claim (claiming itself moves the item into
            // the "started" group) so the assertion below isolates what
            // `handle_duplicate_pr`'s open-PR branch does, not what
            // claiming already did.
            let state_id_after_claim = mcp
                .with_backend_db(|conn| agentflare_backend::item::get(conn, &item.id))
                .unwrap()
                .unwrap()
                .state_id;

            let mut guard = ClaimGuard::new(&mcp, &item.id);
            let mut log = Vec::new();
            let outcome = handle_duplicate_pr(
                &mcp,
                &item.id,
                &item,
                &repo_root,
                &open_duplicate_pr(7),
                None,
                &mut guard,
                &mut log,
            );
            assert_eq!(outcome.exit_code, 0);

            let refreshed = mcp
                .with_backend_db(|conn| agentflare_backend::item::get(conn, &item.id))
                .unwrap()
                .unwrap();
            assert_eq!(
                refreshed.state_id, state_id_after_claim,
                "an open duplicate must not change the item's state"
            );

            let comments = mcp
                .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
                .unwrap()
                .unwrap();
            assert!(
                comments
                    .iter()
                    .any(|c| c.body.contains("needs human review") && c.body.contains("#7")),
                "expected a needs-human-review comment naming the open PR, got: {comments:?}"
            );
        });
    }
