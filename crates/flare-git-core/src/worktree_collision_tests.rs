//! Item #339's worktree-branch collision classifier
//! (`is_worktree_branch_collision`, `collision_owner_seq`): a duplicate
//! item's repair dispatch dying on `fatal: 'task/330-…' is already used by
//! worktree at '…'` is a routing problem, not a repair failure. Split out of
//! `worktree_tests.rs` to keep that file under the LOC gate
//! (`scripts/loc-gate.sh`), mirroring `worktree_heal_tests.rs`.

use super::*;

#[test]
fn branch_collision_matches_already_used_by_worktree_but_not_races() {
    // Item #339's observed failure: a duplicate item's repair dispatch
    // colliding with the branch owner's live checkout.
    assert!(is_worktree_branch_collision(
        "Preparing worktree (checking out 'task/330-fix-thing'); fatal: \
         'task/330-fix-thing' is already used by worktree at \
         '/repo/.worktrees/task/330'"
    ));
    assert!(!is_worktree_branch_collision(
        "could not lock config file .git/config: File exists"
    ));
    assert!(!is_worktree_branch_collision("not a git repository"));
    assert!(!is_worktree_branch_collision(""));

    assert_eq!(
        collision_owner_seq(
            "fatal: 'task/330-fix-thing' is already used by worktree at \
             '/repo/.worktrees/task/330'"
        ),
        Some(330)
    );
    assert_eq!(collision_owner_seq("not a git repository"), None);
    assert_eq!(collision_owner_seq(""), None);
}
