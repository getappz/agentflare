use super::*;

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn read_only_subcommands_pass_through() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "status",
            &[],
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
    assert_eq!(
        classify_pure(
            "log",
            &args(&["-5"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn ordinary_mutating_subcommands_pass_through() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "commit",
            &args(&["-m", "x"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
    assert_eq!(
        classify_pure(
            "reset",
            &args(&["HEAD~1"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn unknown_subcommand_passes_through_by_default() {
    let policy = ResolvedGitShimPolicy::baseline();
    // Fail-open: this shim must never block a subcommand it hasn't
    // been explicitly taught to deny.
    assert_eq!(
        classify_pure(
            "some-future-subcommand",
            &[],
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
    assert_eq!(
        classify_pure(
            "submodule",
            &args(&["update"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
    assert_eq!(
        classify_pure(
            "bisect",
            &args(&["start"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
    assert_eq!(
        classify_pure(
            "lfs",
            &args(&["pull"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn plumbing_commands_are_denied() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert!(matches!(
        classify_pure(
            "update-index",
            &[],
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Deny { .. }
    ));
    assert!(matches!(
        classify_pure(
            "apply",
            &[],
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Deny { .. }
    ));
}

#[test]
fn worktree_is_denied() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert!(matches!(
        classify_pure(
            "worktree",
            &args(&["add", "../x"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Deny { .. }
    ));
}

#[test]
fn worktree_remove_is_denied() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert!(matches!(
        classify_pure(
            "worktree",
            &args(&["remove", "../x"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Deny { .. }
    ));
}

#[test]
fn worktree_teardown_deny_points_at_the_cleanup_tool() {
    // Item #441 / vent #350: an agent denied mid-teardown needs the
    // exact cleanup action, not the provisioning call.
    let policy = ResolvedGitShimPolicy::baseline();
    for sub in ["remove", "prune"] {
        let d = classify_pure(
            "worktree",
            &args(&[sub, "../x"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy,
        );
        let Disposition::Deny { reason } = d else {
            panic!("expected deny for worktree {sub}");
        };
        assert!(reason.contains("check_merge"), "{reason}");
        // The CLI it names must be the real one -- `src/cli/git.rs`'s
        // `worktree_teardown_deny_names_a_parsable_cli` parses this same
        // constant against the clap definition, so the two can't drift.
        assert!(reason.contains(WORKTREE_PRUNE_COMMAND), "{reason}");
    }
}

#[test]
fn worktree_provision_deny_points_at_the_claim_tool() {
    let policy = ResolvedGitShimPolicy::baseline();
    let d = classify_pure(
        "worktree",
        &args(&["add", "../x"]),
        "master",
        &TrustRootTouch::Clean,
        false,
        true,
        &policy,
    );
    let Disposition::Deny { reason } = d else {
        panic!("expected deny for worktree add");
    };
    assert!(reason.contains("item(action=\"claim\""), "{reason}");
}

#[test]
fn worktree_list_is_passthrough() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "worktree",
            &args(&["list"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn worktree_prune_dry_run_is_passthrough() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "worktree",
            &args(&["prune", "--dry-run"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn worktree_prune_without_dry_run_is_denied() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert!(matches!(
        classify_pure(
            "worktree",
            &args(&["prune"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Deny { .. }
    ));
}

#[test]
fn checkout_to_protected_branch_is_denied() {
    // Dirty tree: this is the genuinely risky case (uncommitted work
    // could be stranded), so the checkout is still blocked.
    let policy = ResolvedGitShimPolicy::baseline();
    let d = classify_pure(
        "checkout",
        &args(&["master"]),
        "master",
        &TrustRootTouch::Clean,
        false,
        false,
        &policy,
    );
    assert!(matches!(d, Disposition::Deny { .. }));
}

#[test]
fn checkout_to_protected_branch_on_a_clean_tree_passes_through() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "checkout",
            &args(&["master"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy,
        ),
        Disposition::Passthrough
    );
    assert_eq!(
        classify_pure(
            "switch",
            &args(&["master"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy,
        ),
        Disposition::Passthrough
    );
}

#[test]
fn switch_to_feature_branch_passes_through() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "switch",
            &args(&["feature/x"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn checkout_with_no_target_arg_passes_through() {
    let policy = ResolvedGitShimPolicy::baseline();
    // `git switch -` (previous branch) — nothing to protect against.
    assert_eq!(
        classify_pure(
            "switch",
            &args(&["-"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn push_touching_trust_root_on_feature_branch_passes_through() {
    let policy = ResolvedGitShimPolicy::baseline();
    // A PR-review gate still applies before this reaches the default
    // branch — same reasoning as any other feature-branch push.
    assert_eq!(
        classify_pure(
            "push",
            &args(&["origin", "feature/x"]),
            "master",
            &TrustRootTouch::Touched(vec!["Cargo.toml".to_string()]),
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn push_touching_trust_root_on_default_branch_is_denied() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert!(matches!(
        classify_pure(
            "push",
            &args(&["origin", "master"]),
            "master",
            &TrustRootTouch::Touched(vec!["Cargo.toml".to_string()]),
            true,
            true,
            &policy
        ),
        Disposition::Deny { .. }
    ));
}

#[test]
fn push_not_touching_trust_root_passes_through() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "push",
            &args(&["origin", "feature/x"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn push_of_default_branch_is_denied_even_without_trust_root_changes() {
    let policy = ResolvedGitShimPolicy::baseline();
    // Enforce PR-only: pushing the default branch straight to a remote is
    // blocked regardless of what the diff touches.
    assert!(matches!(
        classify_pure(
            "push",
            &args(&["origin", "master"]),
            "master",
            &TrustRootTouch::Clean,
            true,
            true,
            &policy
        ),
        Disposition::Deny { .. }
    ));
}

#[test]
fn push_of_feature_branch_is_not_a_default_branch_push() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "push",
            &args(&["origin", "feature/x"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn branch_delete_of_protected_branch_is_denied() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert!(matches!(
        classify_pure(
            "branch",
            &args(&["-D", "master"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Deny { .. }
    ));
    assert!(matches!(
        classify_pure(
            "branch",
            &args(&["--delete", "master"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Deny { .. }
    ));
}

#[test]
fn branch_rename_of_protected_branch_is_denied() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert!(matches!(
        classify_pure(
            "branch",
            &args(&["-M", "master", "renamed"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Deny { .. }
    ));
}

#[test]
fn branch_delete_of_feature_branch_passes_through() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "branch",
            &args(&["-D", "feature/x"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn branch_listing_and_creation_pass_through() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "branch",
            &[],
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
    assert_eq!(
        classify_pure(
            "branch",
            &args(&["feature/new"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn would_detach_head_true_for_a_non_branch_checkout_target() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    // A commit sha (via HEAD) is not a branch name -- checking it out
    // implicitly detaches.
    let sha = crate::shell::run_in(&repo.path, &["rev-parse", "HEAD"]).unwrap();
    assert!(would_detach_head(&repo.path, "checkout", &args(&[&sha])));
}

#[test]
fn would_detach_head_false_for_an_existing_branch_checkout_target() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    crate::shell::run_in(&repo.path, &["branch", "feature/x"]).unwrap();
    assert!(!would_detach_head(
        &repo.path,
        "checkout",
        &args(&["feature/x"])
    ));
}

#[test]
fn would_detach_head_false_for_path_restore_form() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    let sha = crate::shell::run_in(&repo.path, &["rev-parse", "HEAD"]).unwrap();
    assert!(!would_detach_head(
        &repo.path,
        "checkout",
        &args(&[&sha, "--", "some-file.txt"])
    ));
}

#[test]
fn branch_create_forms_are_recognized() {
    assert!(is_branch_create("checkout", &args(&["-b", "feature/x"])));
    assert!(is_branch_create("checkout", &args(&["-B", "feature/x"])));
    assert!(is_branch_create("switch", &args(&["-c", "feature/x"])));
    assert!(is_branch_create("switch", &args(&["-C", "feature/x"])));
    assert!(!is_branch_create("checkout", &args(&["feature/x"])));
    assert!(!is_branch_create(
        "checkout",
        &args(&["--detach", "feature/x"])
    ));
    assert!(!is_branch_create("push", &args(&["origin", "master"])));
}

#[test]
fn branch_create_recognizes_attached_and_long_forms() {
    // Attached short-option spellings (`-bname`, not `-b name`).
    assert!(is_branch_create("checkout", &args(&["-bfeature/x"])));
    assert!(is_branch_create("checkout", &args(&["-Bfeature/x"])));
    assert!(is_branch_create("switch", &args(&["-cfeature/x"])));
    assert!(is_branch_create("switch", &args(&["-Cfeature/x"])));
    // `--orphan` on both subcommands.
    assert!(is_branch_create("checkout", &args(&["--orphan", "root"])));
    assert!(is_branch_create("switch", &args(&["--orphan", "root"])));
    // `switch`'s long forms of -c/-C, bare and `=name`.
    assert!(is_branch_create(
        "switch",
        &args(&["--create", "feature/x"])
    ));
    assert!(is_branch_create("switch", &args(&["--create=feature/x"])));
    assert!(is_branch_create(
        "switch",
        &args(&["--force-create", "feature/x"])
    ));
    // Scanning stops at `--`: nothing after it is an option.
    assert!(!is_branch_create("checkout", &args(&["--", "-b"])));
}

#[test]
fn would_detach_head_false_for_branch_creating_checkout() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    // `checkout -b <name>` creates the branch and checks it out -- HEAD
    // is never detached, even though the target branch doesn't exist yet.
    assert!(!would_detach_head(
        &repo.path,
        "checkout",
        &args(&["-b", "feature/x"])
    ));
    assert!(!would_detach_head(
        &repo.path,
        "switch",
        &args(&["-c", "feature/x"])
    ));
}

#[test]
fn would_detach_head_true_for_explicit_detach_flag() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    assert!(would_detach_head(
        &repo.path,
        "checkout",
        &args(&["--detach", "master"])
    ));
    assert!(would_detach_head(
        &repo.path,
        "switch",
        &args(&["--detach", "master"])
    ));
}

#[test]
fn would_detach_head_false_for_plain_switch_to_a_branch() {
    assert!(!would_detach_head(
        std::path::Path::new("."),
        "switch",
        &args(&["feature/x"])
    ));
}

#[test]
fn would_detach_head_false_for_unrelated_subcommands() {
    assert!(!would_detach_head(std::path::Path::new("."), "status", &[]));
}

#[test]
fn is_destructive_flags_reset_hard_and_force_ops() {
    assert!(is_destructive("reset", &args(&["--hard"])));
    assert!(!is_destructive("reset", &args(&["--soft"])));
    assert!(is_destructive("clean", &args(&["-fd"])));
    assert!(!is_destructive("clean", &args(&["-n"])));
    assert!(is_destructive("checkout", &args(&["-f", "master"])));
    assert!(!is_destructive("checkout", &args(&["master"])));
    assert!(!is_destructive("commit", &args(&["-m", "x"])));
}

#[test]
fn is_destructive_flags_clean_regardless_of_flag_form_or_order() {
    // Combined short opts in the order git itself would print them.
    assert!(is_destructive("clean", &args(&["-fd"])));
    // Same combination, opposite order -- git treats "-df" identically to
    // "-fd", but a naive exact-string match on "-fd" alone would miss it.
    assert!(is_destructive("clean", &args(&["-df"])));
    // Long form, not in the original hardcoded list at all.
    assert!(is_destructive("clean", &args(&["--force"])));
    // Separate short flags rather than one combined cluster.
    assert!(is_destructive("clean", &args(&["-f", "-d"])));
    assert!(!is_destructive("clean", &args(&["-n"])));
    assert!(!is_destructive("clean", &args(&["--dry-run"])));
}

#[test]
fn reset_soft_divergence_warning_only_applies_to_soft_or_mixed_reset() {
    // No I/O needed for these -- both bail out before touching the repo.
    assert_eq!(
        reset_soft_divergence_warning(
            std::path::Path::new("."),
            "reset",
            &args(&["origin/master"])
        ),
        None
    );
    assert_eq!(
        reset_soft_divergence_warning(
            std::path::Path::new("."),
            "reset",
            &args(&["--hard", "origin/master"])
        ),
        None
    );
    assert_eq!(
        reset_soft_divergence_warning(std::path::Path::new("."), "commit", &args(&["-m", "x"])),
        None
    );
}

#[test]
fn reset_soft_divergence_warning_none_without_explicit_target() {
    assert_eq!(
        reset_soft_divergence_warning(std::path::Path::new("."), "reset", &args(&["--soft"])),
        None
    );
}

#[test]
fn reset_soft_divergence_warning_fires_when_head_and_target_have_diverged() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    crate::shell::run_in(&repo.path, &["checkout", "-b", "feature"]).unwrap();
    std::fs::write(repo.path.join("feature.txt"), "feature").unwrap();
    crate::shell::run_in(&repo.path, &["add", "feature.txt"]).unwrap();
    crate::shell::run_in(&repo.path, &["commit", "-m", "feature commit"]).unwrap();
    crate::shell::run_in(&repo.path, &["checkout", "master"]).unwrap();
    std::fs::write(repo.path.join("master.txt"), "master").unwrap();
    crate::shell::run_in(&repo.path, &["add", "master.txt"]).unwrap();
    crate::shell::run_in(&repo.path, &["commit", "-m", "master commit"]).unwrap();

    let msg = reset_soft_divergence_warning(&repo.path, "reset", &args(&["--soft", "feature"]));
    let msg = msg.expect("HEAD and feature have diverged -- expected a warning");
    assert!(msg.contains("diverged"), "{msg}");
    assert!(msg.contains("feature"), "{msg}");

    let msg = reset_soft_divergence_warning(&repo.path, "reset", &args(&["--mixed", "feature"]));
    assert!(msg.is_some());
}

#[test]
fn reset_soft_divergence_warning_silent_on_clean_fast_forward() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    let base_sha = crate::shell::run_in(&repo.path, &["rev-parse", "HEAD"]).unwrap();
    std::fs::write(repo.path.join("a.txt"), "a").unwrap();
    crate::shell::run_in(&repo.path, &["add", "a.txt"]).unwrap();
    crate::shell::run_in(&repo.path, &["commit", "-m", "second"]).unwrap();

    // `base_sha` is a strict ancestor of HEAD -- a clean fast-forward
    // reset, not a divergence.
    assert_eq!(
        reset_soft_divergence_warning(&repo.path, "reset", &args(&["--soft", &base_sha])),
        None
    );
    // And the reverse direction: HEAD is a strict ancestor of `feature`.
    crate::shell::run_in(&repo.path, &["branch", "feature"]).unwrap();
    crate::shell::run_in(&repo.path, &["reset", "--hard", &base_sha]).unwrap();
    assert_eq!(
        reset_soft_divergence_warning(&repo.path, "reset", &args(&["--soft", "feature"])),
        None
    );
}

fn single_ref(refs: Option<Vec<PushRef>>) -> PushRef {
    let mut refs = refs.expect("push targets must resolve");
    assert_eq!(refs.len(), 1, "{refs:?}");
    refs.remove(0)
}

#[test]
fn pushed_refs_reads_the_refspec_positionally_skipping_leading_flags() {
    // `-u` before remote/refspec previously threw off a fixed-index
    // `args[1]` read, misreading "origin" as the branch being pushed.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    let expect_feature_x = PushRef {
        source: "feature/x".to_string(),
        destination: "feature/x".to_string(),
    };
    assert_eq!(
        single_ref(pushed_refs(
            &repo.path,
            &args(&["-u", "origin", "feature/x"])
        )),
        expect_feature_x
    );
    assert_eq!(
        single_ref(pushed_refs(
            &repo.path,
            &args(&["--force", "origin", "feature/x"])
        )),
        expect_feature_x
    );
    assert_eq!(
        single_ref(pushed_refs(&repo.path, &args(&["origin", "feature/x"]))),
        expect_feature_x
    );
}

#[test]
fn pushed_refs_falls_back_to_current_branch_when_refspec_omitted() {
    // Bare `git push` and `git push <remote>` (no explicit ref) both push
    // the current/tracked branch -- previously these skipped the
    // trust-root check entirely (args.len() >= 2 was false).
    let repo = crate::shell::test_support::init_repo_with_branch("feature/y");
    let expect_feature_y = PushRef {
        source: "feature/y".to_string(),
        destination: "feature/y".to_string(),
    };
    assert_eq!(single_ref(pushed_refs(&repo.path, &[])), expect_feature_y);
    assert_eq!(
        single_ref(pushed_refs(&repo.path, &args(&["origin"]))),
        expect_feature_y
    );
}

#[test]
fn pushed_refs_splits_src_dest_refspecs_instead_of_reading_the_source_for_both_halves() {
    // The item #321 bug: `pushed_branch` used to read only the refspec's
    // source and use that single name for BOTH the trust-root diff and
    // the "does this target the default branch" check. A `src:dest`
    // refspec needs the two kept separate.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    assert_eq!(
        single_ref(pushed_refs(
            &repo.path,
            &args(&["origin", "feature/x:master"])
        )),
        PushRef {
            source: "feature/x".to_string(),
            destination: "master".to_string(),
        }
    );
    assert_eq!(
        single_ref(pushed_refs(
            &repo.path,
            &args(&["origin", "master:feature/x"])
        )),
        PushRef {
            source: "master".to_string(),
            destination: "feature/x".to_string(),
        }
    );
}

#[test]
fn pushed_refs_resolves_every_refspec_in_a_multi_ref_push() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    let refs = pushed_refs(
        &repo.path,
        &args(&["origin", "feature/a", "feature/b:master"]),
    )
    .unwrap();
    assert_eq!(
        refs,
        vec![
            PushRef {
                source: "feature/a".to_string(),
                destination: "feature/a".to_string(),
            },
            PushRef {
                source: "feature/b".to_string(),
                destination: "master".to_string(),
            },
        ]
    );
}

#[test]
fn pushed_refs_all_flag_resolves_every_local_branch_to_itself() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    crate::shell::run_in(&repo.path, &["branch", "feature/x"]).unwrap();
    let mut refs = pushed_refs(&repo.path, &args(&["origin", "--all"])).unwrap();
    refs.sort_by(|a, b| a.destination.cmp(&b.destination));
    assert_eq!(
        refs,
        vec![
            PushRef {
                source: "feature/x".to_string(),
                destination: "feature/x".to_string(),
            },
            PushRef {
                source: "master".to_string(),
                destination: "master".to_string(),
            },
        ]
    );
}

#[test]
fn pushed_refs_mirror_flag_is_unresolvable() {
    // `--mirror` pushes every ref, including deletions of anything not
    // present locally -- a strictly larger surface than `local_branches`
    // enumerates. Approximating it the same way `--all` is handled could
    // miss exactly the kind of write this exists to catch, so it must
    // fail closed (`None`), not silently under-approximate.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    assert_eq!(
        pushed_refs(&repo.path, &args(&["origin", "--mirror"])),
        None
    );
}

#[test]
fn pushed_refs_tags_only_push_has_no_branch_refs() {
    // `git push origin --tags` with no explicit refspec pushes every
    // tag, not a branch -- there's nothing here for branch-protection
    // checks to inspect, and it must not fall back to "current branch"
    // (which `--tags` never touches).
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    assert_eq!(
        pushed_refs(&repo.path, &args(&["origin", "--tags"])),
        Some(Vec::new())
    );
}

#[test]
fn pushed_refs_skips_the_value_of_value_taking_flags() {
    // `-o`/`--push-option`/`--receive-pack`/`--repo`/`--exec` all
    // consume a separate following token. Left unskipped, that token
    // reads as an ordinary positional and throws off the
    // remote/refspec split -- in the worst case (no explicit refspec)
    // the flag's value silently stands in for the branch actually
    // being pushed, so a push of the real current branch (here the
    // default branch) never gets recognized as one.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    let expect_master = PushRef {
        source: "master".to_string(),
        destination: "master".to_string(),
    };
    assert_eq!(
        single_ref(pushed_refs(&repo.path, &args(&["-o", "ci.skip", "origin"]))),
        expect_master
    );
    assert_eq!(
        single_ref(pushed_refs(
            &repo.path,
            &args(&["--receive-pack", "/opt/git/git-receive-pack", "origin"])
        )),
        expect_master
    );
}

#[test]
fn parse_refspec_strips_the_force_marker() {
    // A leading `+` force-marks the whole refspec. Left in place on the
    // no-colon form, "+master" never matches the default branch name
    // "master", letting a force push of the default branch slip the
    // direct-push guard entirely.
    assert_eq!(
        parse_refspec("+master"),
        PushRef {
            source: "master".to_string(),
            destination: "master".to_string(),
        }
    );
    assert_eq!(
        parse_refspec("+feature/x:master"),
        PushRef {
            source: "feature/x".to_string(),
            destination: "master".to_string(),
        }
    );
}

#[test]
fn force_push_of_default_branch_via_no_colon_refspec_is_denied_end_to_end() {
    // `git push origin +master` -- a force push written in the no-colon
    // `+branch` form rather than `--force`. Must be judged the same as
    // an ordinary push of the default branch.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
    crate::shell::run_in(&repo.path, &["commit", "--allow-empty", "-m", "init"]).unwrap();
    let event = classify(&repo.path, "push", &args(&["origin", "+master"]));
    assert!(
        matches!(event.disposition, Disposition::Deny { .. }),
        "{:?}",
        event.disposition
    );
}

#[test]
fn push_with_leading_flags_touching_trust_root_on_feature_branch_passes_through() {
    // End-to-end regression for the classify()-level bug: a flag before
    // remote/refspec must not throw off which branch gets diffed. Once
    // correctly resolved to "feature/z" (not the default branch), a
    // trust-root touch there is allowed — PR review gates it before
    // master.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::write(repo.path.join("Cargo.toml"), "[package]\n").unwrap();
    crate::shell::run_in(&repo.path, &["add", "Cargo.toml"]).unwrap();
    crate::shell::run_in(&repo.path, &["checkout", "-b", "feature/z"]).unwrap();
    crate::shell::run_in(&repo.path, &["commit", "-m", "touch trust root"]).unwrap();
    let event = classify(&repo.path, "push", &args(&["-u", "origin", "feature/z"]));
    assert_eq!(event.disposition, Disposition::Passthrough, "{event:?}");
}

#[test]
fn bare_push_on_default_branch_is_denied_end_to_end() {
    // The common case: `git push` while checked out on the default branch
    // resolves the current branch (master) and must be blocked, PR-only.
    // Needs the `.agentflare` marker now that denies are gated to tracked
    // repos -- this test is about the push-deny logic, not the gate.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
    crate::shell::run_in(&repo.path, &["commit", "--allow-empty", "-m", "init"]).unwrap();
    let event = classify(&repo.path, "push", &[]);
    assert!(
        matches!(event.disposition, Disposition::Deny { .. }),
        "{:?}",
        event.disposition
    );
}

#[test]
fn push_feature_to_master_via_dest_refspec_is_denied_end_to_end() {
    // Item #321's exact bug: `git push origin feature/x:master` writes
    // to the default branch through a `src:dest` refspec. The old
    // `pushed_branch` read only "feature/x" (the source) and checked
    // *that* against the default branch name -- never matching, so this
    // push sailed through and bypassed default-branch protection
    // entirely. The destination, not the source, is what must be
    // checked.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
    crate::shell::run_in(&repo.path, &["checkout", "-b", "feature/x"]).unwrap();
    crate::shell::run_in(&repo.path, &["commit", "--allow-empty", "-m", "work"]).unwrap();
    let event = classify(&repo.path, "push", &args(&["origin", "feature/x:master"]));
    assert!(
        matches!(event.disposition, Disposition::Deny { .. }),
        "{:?}",
        event.disposition
    );
}

#[test]
fn push_master_to_a_feature_ref_via_src_refspec_is_not_denied_end_to_end() {
    // The flip side of the same bug: `git push origin master:feature/x`
    // pushes master's *content* to a non-default remote ref. The old
    // code read "master" (the source) and, since that name matches the
    // default branch, wrongly denied this even though the actual write
    // destination isn't protected at all.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
    let event = classify(&repo.path, "push", &args(&["origin", "master:feature/x"]));
    assert_eq!(event.disposition, Disposition::Passthrough, "{event:?}");
}

#[test]
fn push_all_denied_when_any_local_branch_maps_to_the_default_branch() {
    // `--all` pushes every local branch to its same-named remote ref --
    // if one of those local branches happens to be named the same as
    // the default branch, that ref-pair must be caught too, not just
    // whichever single branch a positional read would have picked up.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
    crate::shell::run_in(&repo.path, &["branch", "feature/x"]).unwrap();
    let event = classify(&repo.path, "push", &args(&["origin", "--all"]));
    assert!(
        matches!(event.disposition, Disposition::Deny { .. }),
        "{:?}",
        event.disposition
    );
}

#[test]
fn worktree_add_is_denied_in_an_agentflare_tracked_repo() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
    std::fs::write(repo.path.join(".agentflare").join("project.json"), "{}").unwrap();
    let event = classify(
        &repo.path,
        "worktree",
        &["add".to_string(), "../x".to_string()],
    );
    assert!(
        matches!(event.disposition, Disposition::Deny { .. }),
        "{:?}",
        event.disposition
    );
}

#[test]
fn worktree_add_passes_through_in_an_untracked_repo() {
    // No `.agentflare/project.json` -- this repo has nothing to do with
    // agentflare's item-tracking system, so the orchestrator-managed
    // rationale doesn't apply and ordinary worktree use must not be blocked.
    // Bounds the walk-up with the repo's own parent as a synthetic home,
    // same technique agentflare_shim::in_scoped_project's own tests use --
    // the real ambient home dir's exact path form isn't something a test
    // should depend on to stay deterministic across platforms/CI runners.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    let event = classify_with_home(
        &repo.path,
        "worktree",
        &["add".to_string(), "../x".to_string()],
        repo.path.parent(),
    );
    assert_eq!(event.disposition, Disposition::Passthrough);
}

#[test]
fn protected_branch_checkout_passes_through_in_an_untracked_repo() {
    // The untracked-repo gate isn't worktree-specific: every deny this
    // policy produces exists for agentflare's own orchestration, so none
    // of it should apply outside a project agentflare actually tracks.
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    let event = classify_with_home(
        &repo.path,
        "checkout",
        &["master".to_string()],
        repo.path.parent(),
    );
    assert_eq!(event.disposition, Disposition::Passthrough);
}

#[test]
fn protected_branch_checkout_is_still_denied_in_a_tracked_repo() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
    // Dirty tree, same as the risky case in `checkout_to_protected_branch_is_denied`
    // -- this test is about the tracked-repo gate, not the dirty-tree
    // check, so it must stay on the deny side of that check too. Staged
    // (not just untracked) so it exercises a tracked modification.
    std::fs::write(repo.path.join("dirty.txt"), "uncommitted").unwrap();
    crate::shell::run_in(&repo.path, &["add", "dirty.txt"]).unwrap();
    let event = classify(&repo.path, "checkout", &["master".to_string()]);
    assert!(
        matches!(event.disposition, Disposition::Deny { .. }),
        "{:?}",
        event.disposition
    );
}

#[test]
fn protected_branch_checkout_passes_through_in_a_tracked_repo_with_a_clean_tree() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
    std::fs::write(repo.path.join(".agentflare").join("project.json"), "{}").unwrap();
    crate::shell::run_in(&repo.path, &["add", "."]).unwrap();
    crate::shell::run_in(&repo.path, &["commit", "-m", "track project"]).unwrap();
    let event = classify(&repo.path, "checkout", &["master".to_string()]);
    assert_eq!(event.disposition, Disposition::Passthrough);
}

#[test]
fn push_trust_root_deny_message_names_only_the_touched_path() {
    let policy = ResolvedGitShimPolicy::baseline();
    let touch = TrustRootTouch::Touched(vec!["Cargo.toml".to_string()]);
    let d = classify_pure(
        "push",
        &args(&["origin", "master"]),
        "master",
        &touch,
        true,
        true,
        &policy,
    );
    let Disposition::Deny { reason } = d else {
        panic!("expected Deny, got {d:?}");
    };
    assert!(reason.contains("Cargo.toml"), "{reason}");
    assert!(
        !reason.contains(".agentflare/"),
        "message must not name paths that weren't actually touched: {reason}"
    );
    assert!(
        !reason.contains(".githooks/"),
        "message must not name paths that weren't actually touched: {reason}"
    );
}

#[test]
fn push_with_unreadable_diff_on_default_branch_denies_with_unknown_message() {
    let policy = ResolvedGitShimPolicy::baseline();
    let d = classify_pure(
        "push",
        &args(&["origin", "master"]),
        "master",
        &TrustRootTouch::Unknown,
        true,
        true,
        &policy,
    );
    let Disposition::Deny { reason } = d else {
        panic!("expected Deny, got {d:?}");
    };
    assert!(reason.contains("could not be verified"), "{reason}");
}

#[test]
fn push_with_unreadable_diff_on_feature_branch_passes_through() {
    let policy = ResolvedGitShimPolicy::baseline();
    assert_eq!(
        classify_pure(
            "push",
            &args(&["origin", "feature/x"]),
            "master",
            &TrustRootTouch::Unknown,
            false,
            true,
            &policy
        ),
        Disposition::Passthrough
    );
}

#[test]
fn malformed_project_local_config_falls_back_to_baseline_without_blocking_git() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
    std::fs::write(repo.path.join(".agentflare").join("project.json"), "{}").unwrap();
    std::fs::write(
        repo.path.join(".agentflare").join("config.toml"),
        "this is not valid toml [[[",
    )
    .unwrap();

    // An ordinary read-only command must still pass through -- a broken
    // config file must never block git operations.
    let event = classify(&repo.path, "status", &[]);
    assert_eq!(
        event.disposition,
        Disposition::Passthrough,
        "{:?}",
        event.disposition
    );
}

#[test]
fn project_local_config_can_relax_a_denied_plumbing_subcommand() {
    let repo = crate::shell::test_support::init_repo_with_branch("master");
    std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
    std::fs::write(repo.path.join(".agentflare").join("project.json"), "{}").unwrap();

    // Baseline: "apply" is in DENIED_PLUMBING_SUBCOMMANDS.
    let before = classify(&repo.path, "apply", &["patch.diff".to_string()]);
    assert!(
        matches!(before.disposition, Disposition::Deny { .. }),
        "{:?}",
        before.disposition
    );

    // Project-local config explicitly allows it. ALLOWED_MUTATING is
    // checked before DENIED_PLUMBING in classify_pure, so this relaxes it.
    std::fs::write(
        repo.path.join(".agentflare").join("config.toml"),
        "[git_shim]\nextra_allowed_mutating_subcommands = [\"apply\"]\n",
    )
    .unwrap();

    let after = classify(&repo.path, "apply", &["patch.diff".to_string()]);
    assert_eq!(
        after.disposition,
        Disposition::Passthrough,
        "{:?}",
        after.disposition
    );
}
