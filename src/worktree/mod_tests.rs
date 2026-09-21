    use super::*;

    #[test]
    fn pr_body_uses_the_summary_when_given_one() {
        assert_eq!(
            pr_body("item-1", Some("Fixed the race by adding a mutex.")),
            "Fixed the race by adding a mutex."
        );
    }

    #[test]
    fn pr_body_trims_the_summary() {
        assert_eq!(
            pr_body("item-1", Some("  Fixed the race.  \n")),
            "Fixed the race."
        );
    }

    #[test]
    fn pr_body_falls_back_to_the_placeholder_when_summary_is_none() {
        assert_eq!(
            pr_body("item-1", None),
            "Auto-opened on `item done` for item-1."
        );
    }

    #[test]
    fn pr_body_falls_back_to_the_placeholder_when_summary_is_blank() {
        assert_eq!(
            pr_body("item-1", Some("   ")),
            "Auto-opened on `item done` for item-1."
        );
    }

    #[test]
    fn pr_footer_names_agent_machine_and_item() {
        assert_eq!(
            pr_footer("claude-code", "kumar-laptop", 42, "item-uuid"),
            "---\n_Opened by `claude-code` on **kumar-laptop** for item #42 via agentflare._\n\
             <!-- agentflare-item-id: item-uuid -->"
        );
    }

    /// Mirrors what `amannn/action-semantic-pull-request` (configured in
    /// `.github/workflows/pr-title.yml`) checks: title starts with one of
    /// `CONVENTIONAL_TYPES`, optionally scoped, followed by `: `.
    fn satisfies_pr_title_check(title: &str) -> bool {
        let Some((head, rest)) = title.split_once(':') else {
            return false;
        };
        if !rest.starts_with(' ') || rest.trim().is_empty() {
            return false;
        }
        let type_token = head.split('(').next().unwrap_or(head);
        CONVENTIONAL_TYPES.contains(&type_token)
    }

    #[test]
    fn conventional_pr_title_passes_through_an_already_valid_prefix() {
        assert_eq!(
            conventional_pr_title("fix: correct off-by-one in pagination"),
            "fix: correct off-by-one in pagination"
        );
    }

    #[test]
    fn conventional_pr_title_lowercases_an_existing_valid_prefix() {
        assert_eq!(
            conventional_pr_title("Feat: support nested worktrees"),
            "feat: support nested worktrees"
        );
    }

    #[test]
    fn conventional_pr_title_preserves_an_existing_scope() {
        assert_eq!(
            conventional_pr_title("Fix(worktree): don't leak file handles"),
            "fix(worktree): don't leak file handles"
        );
    }

    #[test]
    fn conventional_pr_title_maps_bugfix_prefix_to_fix() {
        assert_eq!(
            conventional_pr_title(
                "Bugfix: detect_review_only doesn't classify design-spec tasks as no-code"
            ),
            "fix: detect_review_only doesn't classify design-spec tasks as no-code"
        );
    }

    #[test]
    fn conventional_pr_title_maps_feature_prefix_to_feat() {
        assert_eq!(
            conventional_pr_title(
                "Feature: add WorkflowStatus::Waiting for Sleep/SleepUntil/WaitEvent suspension"
            ),
            "feat: add WorkflowStatus::Waiting for Sleep/SleepUntil/WaitEvent suspension"
        );
    }

    #[test]
    fn conventional_pr_title_falls_back_to_a_keyword_scan_for_plain_english() {
        assert_eq!(
            conventional_pr_title("improve documentation for the review command"),
            "docs: improve documentation for the review command"
        );
    }

    #[test]
    fn conventional_pr_title_defaults_to_chore_when_nothing_matches() {
        assert_eq!(
            conventional_pr_title("bump vendored dependency versions"),
            "chore: bump vendored dependency versions"
        );
    }

    #[test]
    fn conventional_pr_title_always_satisfies_the_ci_check() {
        for name in [
            "fix: correct off-by-one in pagination",
            "Feat: support nested worktrees",
            "Fix(worktree): don't leak file handles",
            "Bugfix: detect_review_only doesn't classify design-spec tasks as no-code",
            "Feature: add WorkflowStatus::Waiting for Sleep/SleepUntil/WaitEvent suspension",
            "stop the daemon from double-dispatching the same item",
            "bump vendored dependency versions",
            "Add support for Cline CLI",
            "Refactor the worktree module",
            "Improve docs for the review command",
        ] {
            let title = conventional_pr_title(name);
            assert!(
                satisfies_pr_title_check(&title),
                "title {title:?} (from {name:?}) does not satisfy the PR title check"
            );
        }
    }

    #[test]
    fn relabel_pr_completed_is_a_noop_without_a_resolvable_remote() {
        let dir = tempfile::tempdir().unwrap();
        let item = agentflare_backend::item::Item {
            id: "item-1".into(),
            project_id: "p".into(),
            state_id: "s".into(),
            name: "n".into(),
            description: String::new(),
            priority: "none".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id: 9,
            sort_order: 0.0,
            started_at: None,
            completed_at: None,
            archived_at: None,
            external_source: None,
            external_id: None,
            metadata: "{}".into(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
            start_date: None,
            due_date: None,
        };
        // No git repo at `dir.path()`, so `RepoId::resolve_from_remote`
        // returns `None` -- must return without panicking.
        relabel_pr_completed(&item, dir.path());
    }

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

    #[test]
    fn pr_number_from_metadata_reads_the_stored_pr_number() {
        let item = item_with_metadata(191, r#"{"pr":{"number":619,"branch":"b"}}"#);
        assert_eq!(pr_number_from_metadata(&item), Some(619));
    }

    #[test]
    fn pr_number_from_metadata_is_none_when_absent() {
        let item = item_with_metadata(191, r#"{"size":"S"}"#);
        assert_eq!(pr_number_from_metadata(&item), None);
    }

    #[test]
    fn pr_number_from_metadata_is_none_for_non_object_metadata() {
        let item = item_with_metadata(191, "not json");
        assert_eq!(pr_number_from_metadata(&item), None);
    }

    // Item #191: `check_merge` reported "PR not merged yet" for a PR that
    // was demonstrably merged, because `resolve_item_task_branch`
    // reconstructed a branch name that no longer matched what the PR was
    // actually opened against. With `metadata.pr.number` set, `is_pr_merged`
    // must go straight to `pulls::get` by number and never touch branch
    // reconstruction at all.
    #[test]
    fn is_pr_merged_uses_metadata_pr_number_even_when_branch_name_would_not_resolve() {
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"{"number":619,"html_url":"u","state":"closed","title":"t","merged_at":"2026-08-20T00:00:00Z"}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let item = item_with_metadata(
            191,
            r#"{"pr":{"number":619,"branch":"task/191-opencode-agentflare-work-dispatch-doesn"}}"#,
        );

        assert!(is_pr_merged_impl(
            &item,
            Path::new("/does/not/exist"),
            &client,
            &repo
        ));

        let reqs = server.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, "/repos/o/r/pulls/619");
    }

    #[test]
    fn is_pr_merged_falls_back_to_branch_heuristic_when_metadata_pr_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let item = item_with_metadata(63, "{}");
        let branch = flare_git_core::worktree::resolve_item_task_branch(&item, dir.path());
        let body = format!(
            "---\\n_Opened by `claude-code` on **box** for item #{} via agentflare._",
            item.sequence_id
        );
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                &format!(
                    r#"[{{"number":5,"html_url":"u","state":"closed","title":"t","merged_at":"2026-08-20T00:00:00Z","head":{{"ref":"{branch}","sha":"abc"}},"body":"{body}"}}]"#
                ),
            ),
        ]);
        let client = server.client(None);
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };

        assert!(is_pr_merged_impl(&item, dir.path(), &client, &repo));

        let reqs = server.requests();
        assert_eq!(
            reqs[0].path,
            "/repos/o/r/pulls?state=all&per_page=100&page=1"
        );
    }

    #[test]
    fn pr_ci_status_uses_metadata_pr_number_to_report_merged() {
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"{"number":619,"html_url":"u","state":"closed","title":"t","merged_at":"2026-08-20T00:00:00Z"}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let item = item_with_metadata(191, r#"{"pr":{"number":619,"branch":"whatever"}}"#);

        let status = pr_ci_status_impl(&item, Path::new("/does/not/exist"), &client, &repo);

        assert!(matches!(status, PrCiStatus::Merged));
        assert_eq!(server.requests()[0].path, "/repos/o/r/pulls/619");
    }

    #[test]
    fn pr_ci_status_reports_behind_before_ever_fetching_check_runs() {
        // GitHub's own mergeable_state == "behind": mergeable, no conflict,
        // just missing commits the base branch gained since. Only one
        // request should fire -- the PR fetch itself -- confirming this is
        // checked before `list_check_runs` would otherwise be called (item
        // #197's follow-up: no point fetching CI status for checks the
        // branch update is about to invalidate anyway).
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"{"number":621,"html_url":"u","state":"open","title":"t","mergeable":true,"mergeable_state":"behind"}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let item = item_with_metadata(195, r#"{"pr":{"number":621,"branch":"whatever"}}"#);

        let status = pr_ci_status_impl(&item, Path::new("/does/not/exist"), &client, &repo);

        assert!(matches!(status, PrCiStatus::Behind { number: 621 }));
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn pr_ci_status_reports_conflicting_before_ever_fetching_check_runs() {
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"{"number":622,"html_url":"u","state":"open","title":"t","mergeable":false,"mergeable_state":"dirty"}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        let item = item_with_metadata(196, r#"{"pr":{"number":622,"branch":"whatever"}}"#);

        let status = pr_ci_status_impl(&item, Path::new("/does/not/exist"), &client, &repo);

        assert!(matches!(status, PrCiStatus::Conflicting { number: 622 }));
        assert_eq!(server.requests().len(), 1);
    }

    #[test]
    fn update_branch_pr_succeeds_on_a_clean_update() {
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(202, r#"{"message":"Updating"}"#),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        assert!(update_branch_pr(&client, &repo, 7));
    }

    // Idempotency invariant (task #198): whether the branch was already
    // brought up to date by a previous sweep tick, a concurrent daemon, or a
    // human clicking "Update branch" by hand, GitHub answers a repeat
    // update-branch call with an error rather than a silent success --
    // `update_branch_pr` must turn that into a plain `false` (log and skip),
    // never a panic or a retry loop, so a stale `Behind` verdict from a
    // batched snapshot is always safe to act on twice.
    #[test]
    fn update_branch_pr_returns_false_and_does_not_panic_when_already_up_to_date() {
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                422,
                r#"{"message":"Branch is already up-to-date"}"#,
            ),
        ]);
        let client = server.client(Some("tok"));
        let repo = crate::github::RepoId {
            owner: "o".into(),
            repo: "r".into(),
        };
        assert!(!update_branch_pr(&client, &repo, 7));
    }

    fn check(
        name: &str,
        status: &str,
        conclusion: Option<&str>,
    ) -> crate::github::models::CheckRun {
        crate::github::models::CheckRun {
            name: name.into(),
            status: status.into(),
            conclusion: conclusion.map(str::to_string),
        }
    }

    fn batch_data(
        merged: bool,
        mergeable: Option<bool>,
        mergeable_state: Option<&str>,
        checks: Vec<crate::github::models::CheckRun>,
        labels: Vec<String>,
    ) -> crate::github::graphql::BatchPrData {
        crate::github::graphql::BatchPrData {
            merged,
            mergeable,
            mergeable_state: mergeable_state.map(str::to_string),
            checks,
            labels,
        }
    }

    #[test]
    fn pr_ci_status_from_batch_reports_merged_before_looking_at_anything_else() {
        let data = batch_data(true, Some(true), Some("behind"), vec![], vec![]);
        assert!(matches!(
            pr_ci_status_from_batch(101, &data),
            PrCiStatus::Merged
        ));
    }

    #[test]
    fn pr_ci_status_from_batch_reports_behind_before_checks() {
        let data = batch_data(false, Some(true), Some("behind"), vec![], vec![]);
        assert!(matches!(
            pr_ci_status_from_batch(101, &data),
            PrCiStatus::Behind { number: 101 }
        ));
    }

    #[test]
    fn pr_ci_status_from_batch_reports_conflicting_before_checks() {
        let data = batch_data(false, Some(false), Some("dirty"), vec![], vec![]);
        assert!(matches!(
            pr_ci_status_from_batch(101, &data),
            PrCiStatus::Conflicting { number: 101 }
        ));
    }

    #[test]
    fn pr_ci_status_from_batch_reports_passing_with_labels_when_all_checks_succeed() {
        let data = batch_data(
            false,
            Some(true),
            Some("clean"),
            vec![check("build", "completed", Some("success"))],
            vec!["status:pr:approved".into()],
        );
        match pr_ci_status_from_batch(101, &data) {
            PrCiStatus::Passing { number, labels } => {
                assert_eq!(number, 101);
                assert_eq!(labels, vec!["status:pr:approved".to_string()]);
            }
            other => panic!("expected Passing, got {other:?}"),
        }
    }

    #[test]
    fn pr_ci_status_from_batch_reports_failing_checks_by_name() {
        let data = batch_data(
            false,
            Some(true),
            Some("clean"),
            vec![
                check("build", "completed", Some("success")),
                check("clippy", "completed", Some("failure")),
            ],
            vec![],
        );
        match pr_ci_status_from_batch(101, &data) {
            PrCiStatus::Failing {
                number,
                checks,
                labels,
            } => {
                assert_eq!(number, 101);
                assert_eq!(checks, vec!["clippy".to_string()]);
                assert!(labels.is_empty());
            }
            other => panic!("expected Failing, got {other:?}"),
        }
    }

    #[test]
    fn pr_ci_status_from_batch_reports_pending_when_a_check_is_still_running() {
        let data = batch_data(
            false,
            Some(true),
            Some("clean"),
            vec![check("build", "in_progress", None)],
            vec![],
        );
        assert!(matches!(
            pr_ci_status_from_batch(101, &data),
            PrCiStatus::Pending
        ));
    }

    // Regression for item #687: gated jobs (e.g. a `build` matrix behind a
    // `changes` job) may not exist as check-runs yet even though every
    // check-run fetched so far is green, so "0 pending, 0 failed" over an
    // incomplete snapshot is not enough to call it Passing. GitHub's own
    // mergeable_state already accounts for the full required-checks list --
    // "blocked" here means the sweep's snapshot is incomplete.
    #[test]
    fn pr_ci_status_from_batch_reports_pending_when_blocked_despite_green_checks() {
        let data = batch_data(
            false,
            Some(true),
            Some("blocked"),
            vec![check("cla", "completed", Some("success"))],
            vec![],
        );
        assert!(matches!(
            pr_ci_status_from_batch(101, &data),
            PrCiStatus::Pending
        ));
    }

    // Regression for item #587: the "blocked" fix above wasn't enough -- the
    // ping still fired early because GitHub reports "unknown" (mergeability
    // not computed yet at all) in the window right after a push, before it
    // has settled into "blocked". Same incomplete-snapshot race, one tick
    // earlier.
    #[test]
    fn pr_ci_status_from_batch_reports_pending_when_mergeable_state_unknown_despite_green_checks() {
        let data = batch_data(
            false,
            None,
            Some("unknown"),
            vec![check("cla", "completed", Some("success"))],
            vec![],
        );
        assert!(matches!(
            pr_ci_status_from_batch(101, &data),
            PrCiStatus::Pending
        ));
    }

    #[test]
    fn merge_and_persist_pr_identity_merges_without_clobbering_existing_metadata_keys() {
        let conn = agentflare_backend::db::open_in_memory().unwrap();
        let ws = agentflare_backend::workspace::create(
            &conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Test".into(),
                slug: "test".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = agentflare_backend::project::create(
            &conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id.clone(),
                name: "Test".into(),
                identifier: "T".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let state = agentflare_backend::state::list_by_project(&conn, &proj.id)
            .unwrap()
            .into_iter()
            .find(|s| s.is_default)
            .unwrap();
        let item = agentflare_backend::item::create(
            &conn,
            agentflare_backend::item::CreateItem {
                project_id: proj.id,
                state_id: state.id,
                name: "Test Item".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: Some(r#"{"size":"S"}"#.into()),
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
                start_date: None,
                due_date: None,
            },
        )
        .unwrap();

        merge_and_persist_pr_identity(&conn, &item, 619, "task/191-slug");

        let updated = agentflare_backend::item::get(&conn, &item.id).unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
        assert_eq!(metadata["size"], "S");
        assert_eq!(metadata["pr"]["number"], 619);
        assert_eq!(metadata["pr"]["branch"], "task/191-slug");
    }

    fn repo() -> RepoId {
        RepoId {
            owner: "o".into(),
            repo: "r".into(),
        }
    }

    #[test]
    fn is_own_pr_true_for_an_open_pr_tagged_for_this_item() {
        let item = item_with_metadata(259, "{}");
        let body = format!(
            "_Opened by claude-code on box for item #259 via agentflare._\n{}",
            crate::github::pulls::item_id_tag(&item.id)
        );
        let pr: crate::github::models::PullRequest = serde_json::from_value(serde_json::json!({
            "number": 688, "html_url": "u", "state": "open", "title": "t", "body": body
        }))
        .unwrap();
        assert!(is_own_pr(&pr, &item));
    }

    // Item #595: an open PR tagged for another item must not be adopted as
    // this item's PR identity just because it is open.
    #[test]
    fn is_own_pr_false_for_an_open_pr_tagged_for_a_different_item() {
        let item = item_with_metadata(259, "{}");
        let body = format!(
            "_Opened by claude-code on box for item #259 via agentflare._\n{}",
            crate::github::pulls::item_id_tag("some-other-items-uuid")
        );
        let pr: crate::github::models::PullRequest = serde_json::from_value(serde_json::json!({
            "number": 688, "html_url": "u", "state": "open", "title": "t", "body": body
        }))
        .unwrap();
        assert!(!is_own_pr(&pr, &item));
    }

    #[test]
    fn is_own_pr_true_for_a_closed_pr_marked_with_this_items_id() {
        let item = item_with_metadata(259, "{}");
        let pr: crate::github::models::PullRequest = serde_json::from_str(
            r#"{"number":688,"html_url":"u","state":"closed","title":"t",
               "body":"_Opened by claude-code on box for item #259 via agentflare._"}"#,
        )
        .unwrap();
        assert!(is_own_pr(&pr, &item));
    }

    #[test]
    fn is_own_pr_false_for_a_closed_pr_without_this_items_marker() {
        let item = item_with_metadata(259, "{}");
        let pr: crate::github::models::PullRequest =
            serde_json::from_str(r#"{"number":42,"html_url":"u","state":"closed","title":"t"}"#)
                .unwrap();
        assert!(!is_own_pr(&pr, &item));
    }

    // Item #595: a closed/merged PR on the same branch name stamped with the
    // same sequence number but tagged for a different item's UUID must not be
    // trusted as this item's own PR.
    #[test]
    fn is_own_pr_false_for_a_closed_pr_tagged_for_a_different_item() {
        let item = item_with_metadata(259, "{}");
        let body = format!(
            "_Opened by claude-code on box for item #259 via agentflare._\n{}",
            crate::github::pulls::item_id_tag("some-other-items-uuid")
        );
        let pr: crate::github::models::PullRequest = serde_json::from_value(serde_json::json!({
            "number": 688, "html_url": "u", "state": "closed", "title": "t", "body": body
        }))
        .unwrap();
        assert!(!is_own_pr(&pr, &item));
    }

    // Regression for item #261: two workstations independently dispatched to
    // item #259 both saw no PR on the branch and both called `create`.
    // GitHub accepted only one; the loser's `create` failed with a
    // duplicate-branch error. Before this fix, that loser just logged and
    // gave up, so it never recorded a PR at all. It must now recheck
    // `find_existing`, find the winner's PR, and link up with it instead --
    // never a second `create`, never a second label add.
    #[test]
    fn recover_pr_after_failed_create_finds_the_racing_winners_pr() {
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":688,"html_url":"https://gh/o/r/pull/688","state":"open",
                    "title":"t","head":{"ref":"task/259","sha":"abc"},
                    "body":"_Opened by claude-code on box for item #259 via agentflare._"}]"#,
            ),
        ]);
        let client = server.client(None);
        let item = item_with_metadata(259, "{}");

        let recovered =
            recover_pr_after_failed_create(&client, &repo(), "task/259", &item).unwrap();

        assert_eq!(recovered.number, 688);
        assert_eq!(recovered.html_url, "https://gh/o/r/pull/688");
    }

    #[test]
    fn recover_pr_after_failed_create_gives_up_when_no_pr_exists_yet() {
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(200, "[]"),
        ]);
        let client = server.client(None);
        let item = item_with_metadata(259, "{}");

        assert!(recover_pr_after_failed_create(&client, &repo(), "task/259", &item).is_none());
    }

    #[test]
    fn recover_pr_after_failed_create_ignores_an_unrelated_closed_pr_on_the_same_branch() {
        let server = crate::github::test_support::MockServer::start(vec![
            crate::github::test_support::MockResponse::json(
                200,
                r#"[{"number":42,"html_url":"u","state":"closed","title":"t",
                    "head":{"ref":"task/259","sha":"abc"}}]"#,
            ),
        ]);
        let client = server.client(None);
        let item = item_with_metadata(259, "{}");

        assert!(recover_pr_after_failed_create(&client, &repo(), "task/259", &item).is_none());
    }
