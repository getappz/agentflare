//! Path-scope classification for claim-aware git enforcement (QuorumGit
//! pattern adoption, item #234) — orthogonal to `classify`'s subcommand
//! policy. The call site (`flare-git-shim`, and eventually the opencode
//! `tool.execute.before` plugin) runs this alongside `classify()` for
//! `commit`/`push`, using live claim data `classify_pure` has no access to.
//!
//! Unscoped claims (no `scope` declared, or `["**"]`) never generate an
//! `Overlapping` verdict — only a claim with a genuinely narrower declared
//! scope is enforced against other agents. This keeps every claim made
//! before this feature shipped (and every claim that never bothers to
//! declare a scope) from silently blocking someone else's unrelated work in
//! the same repo, which is the normal case: many claims coexist per repo,
//! one per target.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeVerdict {
    /// No changed paths to classify.
    Clear,
    /// Changed paths present but none fall inside any enforced scope.
    Related,
    /// A changed path falls inside another live claim's declared scope.
    Overlapping {
        owner: String,
        target: String,
        scope: String,
    },
    /// The invoker holds a live claim but is committing/pushing from the
    /// canonical checkout rather than that claim's own worktree.
    OutOfTree { target: String },
}

/// One other agent's live claim, as relevant to scope classification.
#[derive(Debug, Clone)]
pub struct ClaimScope {
    pub target: String,
    pub owner: String,
    /// Empty or `["**"]` — the back-compat default — is never enforced.
    pub scopes: Vec<String>,
}

/// The portion of a glob before its first wildcard character.
#[must_use]
pub fn literal_prefix(glob: &str) -> &str {
    glob.find(['*', '?', '[']).map_or(glob, |i| &glob[..i])
}

/// Conservative literal-prefix overlap test — QuorumGit's rule: compare
/// each glob's literal prefix against the other's, erring toward flagging
/// rather than missing a real overlap.
#[must_use]
pub fn globs_overlap(a: &str, b: &str) -> bool {
    let (pa, pb) = (literal_prefix(a), literal_prefix(b));
    pa.starts_with(pb) || pb.starts_with(pa)
}

/// `true` if `scopes` is the back-compat default (unscoped) and so must
/// never be used to deny another agent.
#[must_use]
pub fn scope_is_wildcard_or_empty(scopes: &[String]) -> bool {
    scopes.is_empty() || scopes.iter().all(|s| s == "**")
}

fn path_matches_scope(path: &str, scope: &str) -> bool {
    scope == "**" || path.starts_with(literal_prefix(scope))
}

/// Classifies a set of changed paths against live claims held by other
/// agents, plus the invoker's own claim/worktree state.
///
/// `own_target` is the invoker's own live claim in this repo, if any.
/// `in_own_worktree` is whether the invoker is currently inside a linked
/// worktree (irrelevant when `own_target` is `None`). `others` are OTHER
/// agents' live claims in this repo.
#[must_use]
pub fn classify_scopes(
    changed_paths: &[String],
    own_target: Option<&str>,
    in_own_worktree: bool,
    others: &[ClaimScope],
) -> ScopeVerdict {
    if changed_paths.is_empty() {
        return ScopeVerdict::Clear;
    }
    if let Some(target) = own_target
        && !in_own_worktree
    {
        return ScopeVerdict::OutOfTree {
            target: target.to_string(),
        };
    }
    for claim in others {
        if scope_is_wildcard_or_empty(&claim.scopes) {
            continue;
        }
        for path in changed_paths {
            if let Some(scope) = claim.scopes.iter().find(|s| path_matches_scope(path, s)) {
                return ScopeVerdict::Overlapping {
                    owner: claim.owner.clone(),
                    target: claim.target.clone(),
                    scope: scope.clone(),
                };
            }
        }
    }
    ScopeVerdict::Related
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn claim(target: &str, owner: &str, scopes: &[&str]) -> ClaimScope {
        ClaimScope {
            target: target.to_string(),
            owner: owner.to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn no_changed_paths_is_clear() {
        assert_eq!(classify_scopes(&[], None, true, &[]), ScopeVerdict::Clear);
    }

    #[test]
    fn disjoint_scopes_never_interfere() {
        let others = [claim("item#1", "a:1", &["crates/foo/"])];
        assert_eq!(
            classify_scopes(&paths(&["crates/bar/src/lib.rs"]), None, true, &others),
            ScopeVerdict::Related
        );
    }

    #[test]
    fn overlapping_scope_is_denied() {
        let others = [claim("item#1", "a:1", &["crates/foo/"])];
        assert_eq!(
            classify_scopes(&paths(&["crates/foo/src/lib.rs"]), None, true, &others),
            ScopeVerdict::Overlapping {
                owner: "a:1".to_string(),
                target: "item#1".to_string(),
                scope: "crates/foo/".to_string(),
            }
        );
    }

    #[test]
    fn unscoped_other_claim_never_blocks() {
        // The back-compat default (no scope declared) must not deny
        // unrelated work just because *some* claim exists in the repo.
        let others = [claim("item#1", "a:1", &[])];
        assert_eq!(
            classify_scopes(&paths(&["crates/foo/src/lib.rs"]), None, true, &others),
            ScopeVerdict::Related
        );
        let wildcard = [claim("item#1", "a:1", &["**"])];
        assert_eq!(
            classify_scopes(&paths(&["crates/foo/src/lib.rs"]), None, true, &wildcard),
            ScopeVerdict::Related
        );
    }

    #[test]
    fn own_claim_in_canonical_checkout_is_out_of_tree() {
        assert_eq!(
            classify_scopes(&paths(&["src/main.rs"]), Some("item#2"), false, &[]),
            ScopeVerdict::OutOfTree {
                target: "item#2".to_string()
            }
        );
    }

    #[test]
    fn own_claim_inside_its_worktree_passes() {
        assert_eq!(
            classify_scopes(&paths(&["src/main.rs"]), Some("item#2"), true, &[]),
            ScopeVerdict::Related
        );
    }

    #[test]
    fn own_claim_with_no_changes_never_blocks() {
        // e.g. `git commit --allow-empty` — nothing actually moved, so
        // there is nothing to be "out of tree" about.
        assert_eq!(
            classify_scopes(&[], Some("item#2"), false, &[]),
            ScopeVerdict::Clear
        );
    }

    #[test]
    fn out_of_tree_takes_priority_over_overlap_checks() {
        let others = [claim("item#1", "a:1", &["src/"])];
        assert_eq!(
            classify_scopes(&paths(&["src/main.rs"]), Some("item#2"), false, &others),
            ScopeVerdict::OutOfTree {
                target: "item#2".to_string()
            }
        );
    }

    #[test]
    fn globs_overlap_is_symmetric_and_conservative() {
        assert!(globs_overlap("crates/foo/", "crates/foo/src/"));
        assert!(globs_overlap("crates/foo/src/", "crates/foo/"));
        assert!(!globs_overlap("crates/foo/", "crates/bar/"));
        assert!(globs_overlap("crates/foo/**", "crates/foo/src/lib.rs"));
    }

    #[test]
    fn literal_prefix_stops_at_first_wildcard() {
        assert_eq!(literal_prefix("crates/foo/**"), "crates/foo/");
        assert_eq!(literal_prefix("crates/foo/"), "crates/foo/");
        assert_eq!(literal_prefix("**"), "");
    }
}
