//! Command classification: the policy core of the git-shim. Every `git
//! <subcommand> <args>` invocation gets classified into exactly one
//! disposition before the shim decides whether to exec real git.
//!
//! Fail-OPEN by default: a subcommand this policy doesn't explicitly
//! recognize is `Passthrough`, not `Deny`. This is a live shim sitting in
//! front of someone's daily-driver git usage -- it must never block a
//! legitimate operation just because its allowlist hasn't caught up with
//! git's full subcommand surface (submodule, bisect, notes, gc, lfs, ...).
//! Only the specific, deliberately-chosen cases below (protected-branch
//! checkout/switch/delete/rename, trust-root push, low-level plumbing,
//! `worktree`) are ever denied -- those are known and intentional, not
//! "doesn't recognize it". `RedirectToWorktree` exists in the `Disposition` enum for API
//! completeness (mirroring the inspiration project's 4-way model) but v1's
//! policy never produces it — agentflare has no per-agent worktree binding
//! data available at classify time yet.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::branch::{is_protected_branch, resolve_default_branch};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Disposition {
    Passthrough,
    RedirectToWorktree { path: PathBuf },
    SilentExempt,
    Deny { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub subcommand: String,
    pub args: Vec<String>,
    pub disposition: Disposition,
}

/// Trust-root paths a `push` must never carry changes to — agentflare's own
/// enforcement config, not something an agent should be able to push a
/// change to and quietly weaken.
const TRUST_ROOT_PATHS: &[&str] = &[".githooks/", ".agentflare/", "Cargo.toml"];

/// Ordinary, non-destructive read-only subcommands — always `Passthrough`
/// regardless of args.
const READ_ONLY_SUBCOMMANDS: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "blame",
    "shortlog",
    "describe",
    "ls-files",
    "ls-tree",
    "cat-file",
    "grep",
    "reflog",
    "rev-parse",
    "rev-list",
    "symbolic-ref",
    "config",
    "remote",
    "tag",
    "fetch",
    "clone",
    "help",
    "version",
];

/// Ordinary mutating workflow commands, allowed by default — none of these
/// are individually dangerous the way `reset --hard`/`clean -f`/protected-
/// branch checkout/trust-root push are.
const ALLOWED_MUTATING_SUBCOMMANDS: &[&str] = &[
    "add",
    "commit",
    "merge",
    "rebase",
    "pull",
    "cherry-pick",
    "revert",
    "stash",
    "init",
    "restore",
    "reset",
    "clean",
];

/// Low-level plumbing that can bypass the higher-level checks above —
/// denied outright rather than reasoned about case by case.
const DENIED_PLUMBING_SUBCOMMANDS: &[&str] = &[
    "read-tree",
    "update-index",
    "apply",
    "hash-object",
    "mktree",
    "commit-tree",
    "update-ref",
];

/// `true` for the destructive ops that must be snapshotted before they run
/// (see `snapshot::snapshot_before`) — orthogonal to `Disposition`: a
/// destructive command is still `Passthrough`-classified (it's allowed),
/// but the shim binary must snapshot first.
#[must_use]
pub fn is_destructive(subcommand: &str, args: &[String]) -> bool {
    match subcommand {
        "reset" => args.iter().any(|a| a == "--hard"),
        "clean" => args.iter().any(|a| a == "-f" || a == "-fd" || a == "-fx" || a == "-fdx"),
        "checkout" | "switch" => args.iter().any(|a| a == "-f" || a == "--force" || a == "-B"),
        _ => false,
    }
}

/// Pure classification core — no I/O, so it's unit-testable with fixed
/// inputs. `default_branch` is the repo's resolved default branch.
/// `push_touches_trust_root` is pre-resolved by the caller (requires a real
/// `git diff`, hence not something a pure function can determine itself)
/// and is only consulted when `subcommand == "push"`.
#[must_use]
pub fn classify_pure(
    subcommand: &str,
    args: &[String],
    default_branch: &str,
    push_touches_trust_root: bool,
) -> Disposition {
    if READ_ONLY_SUBCOMMANDS.contains(&subcommand) || ALLOWED_MUTATING_SUBCOMMANDS.contains(&subcommand) {
        return Disposition::Passthrough;
    }
    if DENIED_PLUMBING_SUBCOMMANDS.contains(&subcommand) {
        return Disposition::Deny {
            reason: format!(
                "'git {subcommand}' is a low-level plumbing command blocked by the agentflare git shim — it can bypass the checks this shim applies to higher-level commands."
            ),
        };
    }
    match subcommand {
        // Deletion/rename lumped with checkout/switch below: `git branch
        // -D/-M <name>` is a second way to destroy or rename the protected
        // branch's local ref, not covered by the checkout/switch guard.
        // Every other `branch` usage (listing, creating a new branch,
        // --set-upstream-to, ...) stays Passthrough.
        "branch" => {
            let deletes_or_renames = args
                .iter()
                .any(|a| matches!(a.as_str(), "-D" | "-d" | "--delete" | "-M" | "-m" | "--move"));
            if !deletes_or_renames {
                return Disposition::Passthrough;
            }
            let targets: Vec<&str> = args.iter().filter(|a| !a.starts_with('-')).map(String::as_str).collect();
            if targets.iter().any(|t| is_protected_branch(t, Some(default_branch))) {
                Disposition::Deny {
                    reason: "this 'git branch' invocation would delete or rename the repo's default branch — blocked by the agentflare git shim.".to_string(),
                }
            } else {
                Disposition::Passthrough
            }
        }
        "checkout" | "switch" => {
            let Some(target) = args.iter().find(|a| !a.starts_with('-')) else {
                return Disposition::Passthrough; // no target arg (e.g. `git switch -`) — nothing to protect against
            };
            if is_protected_branch(target, Some(default_branch)) {
                Disposition::Deny {
                    reason: format!(
                        "'{target}' is this repo's default branch — direct checkout/switch is blocked by the agentflare git shim. Create an isolated worktree first."
                    ),
                }
            } else {
                Disposition::Passthrough
            }
        }
        "push" => {
            if push_touches_trust_root {
                Disposition::Deny {
                    reason: "this push carries changes to a trust-root path (.githooks/, .agentflare/, or Cargo.toml) — blocked by the agentflare git shim.".to_string(),
                }
            } else {
                Disposition::Passthrough
            }
        }
        "worktree" => Disposition::Deny {
            reason: "'git worktree' is orchestrator-managed by agentflare — use the `item` MCP tool's claim flow instead of calling it directly.".to_string(),
        },
        // Fail-open: anything not explicitly matched above is allowed through
        // unchanged. This shim must never block a git subcommand it simply
        // hasn't been taught about yet.
        _ => Disposition::Passthrough,
    }
}

/// Whether pushing would carry changes to a trust-root path — inspects the
/// diff between `branch` and `target`. Errs toward `true` (blocking) if
/// that diff can't be determined at all: an unreadable diff is not a safe
/// default to let through.
#[must_use]
pub fn push_touches_trust_root(repo_root: &Path, branch: &str, target: &str) -> bool {
    let range = format!("{target}...{branch}");
    match crate::shell::run_in(repo_root, &["diff", "--name-only", &range]) {
        Ok(names) => names
            .lines()
            .any(|f| TRUST_ROOT_PATHS.iter().any(|p| f.starts_with(p))),
        Err(_) => true,
    }
}

/// I/O-resolving entry point: resolves the default branch and (for `push`
/// with a resolvable branch/target pair) whether the push touches a
/// trust-root path, then delegates to `classify_pure`.
#[must_use]
pub fn classify(repo_root: &Path, subcommand: &str, args: &[String]) -> Event {
    let default_branch = resolve_default_branch(repo_root);
    let touches_trust_root = subcommand == "push"
        && args.len() >= 2
        && push_touches_trust_root(repo_root, &args[1], &default_branch);
    let disposition = classify_pure(subcommand, args, &default_branch, touches_trust_root);
    Event {
        subcommand: subcommand.to_string(),
        args: args.to_vec(),
        disposition,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn read_only_subcommands_pass_through() {
        assert_eq!(
            classify_pure("status", &[], "master", false),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure("log", &args(&["-5"]), "master", false),
            Disposition::Passthrough
        );
    }

    #[test]
    fn ordinary_mutating_subcommands_pass_through() {
        assert_eq!(
            classify_pure("commit", &args(&["-m", "x"]), "master", false),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure("reset", &args(&["HEAD~1"]), "master", false),
            Disposition::Passthrough
        );
    }

    #[test]
    fn unknown_subcommand_passes_through_by_default() {
        // Fail-open: this shim must never block a subcommand it hasn't
        // been explicitly taught to deny.
        assert_eq!(
            classify_pure("some-future-subcommand", &[], "master", false),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure("submodule", &args(&["update"]), "master", false),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure("bisect", &args(&["start"]), "master", false),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure("lfs", &args(&["pull"]), "master", false),
            Disposition::Passthrough
        );
    }

    #[test]
    fn plumbing_commands_are_denied() {
        assert!(matches!(
            classify_pure("update-index", &[], "master", false),
            Disposition::Deny { .. }
        ));
        assert!(matches!(
            classify_pure("apply", &[], "master", false),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn worktree_is_denied() {
        assert!(matches!(
            classify_pure("worktree", &args(&["add", "../x"]), "master", false),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn checkout_to_protected_branch_is_denied() {
        let d = classify_pure("checkout", &args(&["master"]), "master", false);
        assert!(matches!(d, Disposition::Deny { .. }));
    }

    #[test]
    fn switch_to_feature_branch_passes_through() {
        assert_eq!(
            classify_pure("switch", &args(&["feature/x"]), "master", false),
            Disposition::Passthrough
        );
    }

    #[test]
    fn checkout_with_no_target_arg_passes_through() {
        // `git switch -` (previous branch) — nothing to protect against.
        assert_eq!(
            classify_pure("switch", &args(&["-"]), "master", false),
            Disposition::Passthrough
        );
    }

    #[test]
    fn push_touching_trust_root_is_denied() {
        assert!(matches!(
            classify_pure("push", &args(&["origin", "feature/x"]), "master", true),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn push_not_touching_trust_root_passes_through() {
        assert_eq!(
            classify_pure("push", &args(&["origin", "feature/x"]), "master", false),
            Disposition::Passthrough
        );
    }

    #[test]
    fn branch_delete_of_protected_branch_is_denied() {
        assert!(matches!(
            classify_pure("branch", &args(&["-D", "master"]), "master", false),
            Disposition::Deny { .. }
        ));
        assert!(matches!(
            classify_pure("branch", &args(&["--delete", "master"]), "master", false),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn branch_rename_of_protected_branch_is_denied() {
        assert!(matches!(
            classify_pure("branch", &args(&["-M", "master", "renamed"]), "master", false),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn branch_delete_of_feature_branch_passes_through() {
        assert_eq!(
            classify_pure("branch", &args(&["-D", "feature/x"]), "master", false),
            Disposition::Passthrough
        );
    }

    #[test]
    fn branch_listing_and_creation_pass_through() {
        assert_eq!(classify_pure("branch", &[], "master", false), Disposition::Passthrough);
        assert_eq!(
            classify_pure("branch", &args(&["feature/new"]), "master", false),
            Disposition::Passthrough
        );
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
}
