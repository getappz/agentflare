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

    #[test]
    fn pick_duplicate_pr_prefers_a_merged_match_over_an_open_one() {
        let picked =
            pick_duplicate_pr(vec![open_duplicate_pr(1), duplicate_pr(2, true)]).unwrap();
        assert_eq!(picked.number, 2);
    }

    #[test]
    fn pick_duplicate_pr_returns_none_for_an_empty_list() {
        assert!(pick_duplicate_pr(vec![]).is_none());
    }

    /// A closed-but-unmerged PR (a previously abandoned attempt, e.g. one
    /// found superseded and closed rather than merged) must not be treated
    /// as an open duplicate — that would permanently block a legitimate
    /// redispatch. `duplicate_pr(_, false)` builds exactly this shape:
    /// `state: "closed"`, `merged_at: None`.
    #[test]
    fn pick_duplicate_pr_ignores_a_closed_unmerged_pr() {
        assert!(pick_duplicate_pr(vec![duplicate_pr(3, false)]).is_none());
    }

    #[test]
    fn pick_duplicate_pr_selects_a_genuinely_open_pr_when_no_merged_match_exists() {
        let picked = pick_duplicate_pr(vec![duplicate_pr(4, false), open_duplicate_pr(5)]).unwrap();
        assert_eq!(picked.number, 5);
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
