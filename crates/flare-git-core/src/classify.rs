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
//! mutating `worktree` subcommands) are ever denied -- those are known and
//! intentional, not "doesn't recognize it". `RedirectToWorktree` exists in
//! the `Disposition` enum for API completeness (mirroring the inspiration
//! project's 4-way model) but v1's policy never produces it — agentflare has
//! no per-agent worktree binding data available at classify time yet.
//!
//! `worktree`'s deny is further scoped: read-only subcommands (`list`,
//! `prune --dry-run`) always pass through regardless of tracking status --
//! decided right here since it only needs `args`. Mutating `worktree`
//! subcommands still classify as `Deny` from this pure function, same as
//! every other deny case above -- but `classify()` (the I/O-resolving
//! wrapper) then downgrades ANY deny to `Passthrough` when the repo isn't
//! actually agentflare-tracked (`agentflare_shim::in_scoped_project`).
//! Every one of this policy's protections exists for agentflare's own
//! orchestration; none of that rationale holds in a project agentflare
//! doesn't track, and this shim is installed globally on PATH, so without
//! that gate it would police ordinary git use in every unrelated project on
//! the machine too.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::branch::{current_branch, is_protected_branch, resolve_default_branch};

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

/// `AGENTFLARE_GIT_TRUST_ROOT_PATHS`, comma-separated, appended to
/// `TRUST_ROOT_PATHS` -- e.g. `".githooks/,policy.toml"`. Empty/unset ->
/// no extra paths.
#[must_use]
pub fn extra_trust_root_paths_from_env() -> Vec<String> {
    std::env::var("AGENTFLARE_GIT_TRUST_ROOT_PATHS")
        .ok()
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Env vars agent CLIs set on themselves -- same catalog `agentflare-shim`
/// gates its own dispatch behind. Used ONLY to scope the canonical-repo
/// mutation guard (see `would_detach_head`) to agent-driven invocations --
/// never to change ordinary git behavior for interactive human use.
const AGENT_ENV_VARS: &[&str] = &[
    "CLAUDECODE",
    "CURSOR_AGENT",
    "CODEX_CLI_SESSION",
    "GEMINI_SESSION",
    "CODEBUDDY",
    "AGENTFLARE_AGENT",
];

/// `true` if any agent-identifying env var is set -- this invocation is
/// (self-reportedly) agent-driven, not an interactive human shell.
#[must_use]
pub fn agent_invocation_detected() -> bool {
    AGENT_ENV_VARS
        .iter()
        .any(|v| std::env::var_os(v).is_some_and(|s| !s.is_empty()))
}

/// `true` if `subcommand`/`args` would detach HEAD -- `git checkout
/// <target>` implicitly detaches when `target` isn't an existing local
/// branch (no `--detach` flag required for that form); `git switch` never
/// silently detaches, only `switch --detach`/`-d` does. `git checkout --
/// <pathspec>` (and any form with `--` before the target) restores files
/// and never touches HEAD at all.
#[must_use]
pub fn would_detach_head(repo_root: &Path, subcommand: &str, args: &[String]) -> bool {
    match subcommand {
        "checkout" => {
            if args.iter().any(|a| a == "--") {
                return false; // path-restore form -- HEAD never moves
            }
            if args.iter().any(|a| a == "--detach") {
                return true;
            }
            let Some(target) = args.iter().find(|a| !a.starts_with('-')) else {
                return false; // e.g. bare `git checkout` -- doesn't move HEAD
            };
            !crate::shell::run_in_ok(
                repo_root,
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{target}"),
                ],
            )
        }
        "switch" => args.iter().any(|a| a == "--detach" || a == "-d"),
        _ => false,
    }
}

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
        "clean" => args.iter().any(|a| {
            a == "--force" || (a.starts_with('-') && !a.starts_with("--") && a.contains('f'))
        }),
        "checkout" | "switch" => args
            .iter()
            .any(|a| a == "-f" || a == "--force" || a == "-B"),
        _ => false,
    }
}

/// Pure classification core — no I/O, so it's unit-testable with fixed
/// inputs. `default_branch` is the repo's resolved default branch.
/// `trust_root_touch` and `push_targets_default_branch` are pre-resolved by
/// the caller (both require resolving the actual pushed branch, hence not
/// something a pure function can determine itself) and are only consulted
/// when `subcommand == "push"`.
#[must_use]
pub fn classify_pure(
    subcommand: &str,
    args: &[String],
    default_branch: &str,
    trust_root_touch: &TrustRootTouch,
    push_targets_default_branch: bool,
) -> Disposition {
    if READ_ONLY_SUBCOMMANDS.contains(&subcommand)
        || ALLOWED_MUTATING_SUBCOMMANDS.contains(&subcommand)
    {
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
            let deletes_or_renames = args.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "-D" | "-d" | "--delete" | "-M" | "-m" | "--move"
                )
            });
            if !deletes_or_renames {
                return Disposition::Passthrough;
            }
            let targets: Vec<&str> = args
                .iter()
                .filter(|a| !a.starts_with('-'))
                .map(String::as_str)
                .collect();
            if targets
                .iter()
                .any(|t| is_protected_branch(t, Some(default_branch)))
            {
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
        // Trust-root touches are only blocked when the push targets the
        // default branch directly. A trust-root change pushed to a feature
        // branch still has to clear a PR review before it reaches the
        // default branch — the same safety net that already applies to
        // every other kind of change — so blocking it here too just forces
        // routine work (adding a crate/dependency) through a manual push
        // every time, without adding real protection over the default-
        // branch guard below.
        "push" => match trust_root_touch {
            TrustRootTouch::Touched(paths) if push_targets_default_branch => Disposition::Deny {
                reason: format!(
                    "this push carries changes to a trust-root path ({}) and targets the repo's default branch '{default_branch}' — blocked by the agentflare git shim. Push a feature/worktree branch and open a PR instead.",
                    paths.join(", ")
                ),
            },
            TrustRootTouch::Touched(_) => Disposition::Passthrough,
            TrustRootTouch::Unknown if push_targets_default_branch => Disposition::Deny {
                reason: format!(
                    "this push's diff against trust-root paths (.githooks/, .agentflare/, Cargo.toml) could not be verified, and it targets the repo's default branch '{default_branch}' — blocked by the agentflare git shim as a precaution."
                ),
            },
            TrustRootTouch::Unknown => Disposition::Passthrough,
            TrustRootTouch::Clean if push_targets_default_branch => Disposition::Deny {
                reason: format!(
                    "pushing the default branch '{default_branch}' to a remote is blocked by the agentflare git shim — push a feature/worktree branch and open a PR instead."
                ),
            },
            TrustRootTouch::Clean => Disposition::Passthrough,
        },
        "worktree" => {
            let is_read_only = match args.first().map(String::as_str) {
                Some("list") => true,
                Some("prune") => args.iter().any(|a| a == "--dry-run"),
                _ => false,
            };
            if is_read_only {
                Disposition::Passthrough
            } else {
                Disposition::Deny {
                    reason: "'git worktree' is orchestrator-managed by agentflare — use the `item` MCP tool's claim flow instead of calling it directly.".to_string(),
                }
            }
        }
        // Fail-open: anything not explicitly matched above is allowed through
        // unchanged. This shim must never block a git subcommand it simply
        // hasn't been taught about yet.
        _ => Disposition::Passthrough,
    }
}

/// Result of checking whether a `push` would carry changes to a trust-root
/// path — `Touched` names exactly the matched path(s) so the shim's deny
/// message doesn't have to fall back to listing every pattern it knows
/// about, forcing the caller to guess which one actually applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustRootTouch {
    Clean,
    Touched(Vec<String>),
    /// The diff couldn't be read at all — fail closed rather than assume
    /// `Clean`, but say so plainly instead of naming paths that were never
    /// actually confirmed.
    Unknown,
}

/// Resolves whether pushing would carry changes to a trust-root path —
/// inspects the diff between `branch` and `target` and names exactly which
/// configured pattern(s) matched. Fails to `Unknown` (still blocks) if that
/// diff can't be determined at all: an unreadable diff is not a safe
/// default to let through, but the caller shouldn't claim to know which
/// path caused it.
#[must_use]
pub fn resolve_trust_root_touch(repo_root: &Path, branch: &str, target: &str) -> TrustRootTouch {
    let extra = extra_trust_root_paths_from_env();
    let range = format!("{target}...{branch}");
    match crate::shell::run_in(repo_root, &["diff", "--name-only", &range]) {
        Ok(names) => {
            let mut matched: Vec<String> = names
                .lines()
                .filter(|f| {
                    TRUST_ROOT_PATHS.iter().any(|p| f.starts_with(p))
                        || extra.iter().any(|p| f.starts_with(p.as_str()))
                })
                .map(str::to_string)
                .collect();
            matched.sort();
            matched.dedup();
            if matched.is_empty() {
                TrustRootTouch::Clean
            } else {
                TrustRootTouch::Touched(matched)
            }
        }
        Err(_) => TrustRootTouch::Unknown,
    }
}

/// Resolves which local branch/ref a `push` invocation would actually
/// push, skipping flags positionally (`-u`, `--force`, `--force-with-lease`,
/// `--tags`, ...) rather than assuming `args[1]` -- a flag before the
/// remote/refspec (e.g. `git push -u origin feature/x`) previously threw
/// off a fixed-index read, misreading the remote name (`"origin"`) as the
/// branch being pushed and either mis-diffing or spuriously denying. Falls
/// back to the current checked-out branch when the refspec is omitted
/// entirely (bare `git push`, or `git push <remote>` with no explicit ref
/// -- both push the current/tracked branch, not something namable from
/// `args` alone) -- this also closes the gap where the single most common
/// push form (`git push`) skipped the trust-root check entirely.
fn pushed_branch(repo_root: &Path, args: &[String]) -> Option<String> {
    let non_flags: Vec<&str> = args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .map(String::as_str)
        .collect();
    let raw = match non_flags.len() {
        0 | 1 => current_branch(repo_root),
        _ => Some(non_flags[1].to_string()),
    }?;
    let branch = raw.split(':').next().unwrap_or(&raw);
    Some(branch.trim_start_matches("refs/heads/").to_string())
}

/// I/O-resolving entry point: resolves the default branch and (for `push`
/// with a resolvable branch/target pair) whether the push touches a
/// trust-root path, then delegates to `classify_pure`.
#[must_use]
pub fn classify(repo_root: &Path, subcommand: &str, args: &[String]) -> Event {
    let default_branch = resolve_default_branch(repo_root);
    // Resolve the actual pushed branch once, then derive both push facts from
    // it: whether it carries trust-root changes and whether it *is* the
    // default branch (direct push blocked in favour of a PR).
    let pushed = (subcommand == "push")
        .then(|| pushed_branch(repo_root, args))
        .flatten();
    let trust_root_touch = pushed
        .as_deref()
        .map(|b| resolve_trust_root_touch(repo_root, b, &default_branch))
        .unwrap_or(TrustRootTouch::Clean);
    let targets_default_branch = pushed
        .as_deref()
        .is_some_and(|b| is_protected_branch(b, Some(&default_branch)));
    let mut disposition = classify_pure(
        subcommand,
        args,
        &default_branch,
        &trust_root_touch,
        targets_default_branch,
    );
    // Every deny above (protected-branch checkout/switch/delete/rename,
    // trust-root push, plumbing block, worktree) exists to protect agentflare's
    // own orchestration in a project it actually tracks. None of that rationale
    // holds in an untracked repo -- this shim is installed globally on PATH, so
    // without this gate it would police ordinary git use in every unrelated
    // project on the machine too, which is worse than the risk it's meant to
    // prevent. `in_scoped_project` is agentflare-shim's own established
    // project-detection walk-up (shared so this doesn't reinvent it).
    if matches!(disposition, Disposition::Deny { .. })
        && !agentflare_shim::in_scoped_project(repo_root, dirs::home_dir().as_deref())
    {
        disposition = Disposition::Passthrough;
    }
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
            classify_pure("status", &[], "master", &TrustRootTouch::Clean, false),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure(
                "log",
                &args(&["-5"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn ordinary_mutating_subcommands_pass_through() {
        assert_eq!(
            classify_pure(
                "commit",
                &args(&["-m", "x"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure(
                "reset",
                &args(&["HEAD~1"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn unknown_subcommand_passes_through_by_default() {
        // Fail-open: this shim must never block a subcommand it hasn't
        // been explicitly taught to deny.
        assert_eq!(
            classify_pure(
                "some-future-subcommand",
                &[],
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure(
                "submodule",
                &args(&["update"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure(
                "bisect",
                &args(&["start"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure(
                "lfs",
                &args(&["pull"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn plumbing_commands_are_denied() {
        assert!(matches!(
            classify_pure("update-index", &[], "master", &TrustRootTouch::Clean, false),
            Disposition::Deny { .. }
        ));
        assert!(matches!(
            classify_pure("apply", &[], "master", &TrustRootTouch::Clean, false),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn worktree_is_denied() {
        assert!(matches!(
            classify_pure(
                "worktree",
                &args(&["add", "../x"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn worktree_remove_is_denied() {
        assert!(matches!(
            classify_pure(
                "worktree",
                &args(&["remove", "../x"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn worktree_list_is_passthrough() {
        assert_eq!(
            classify_pure(
                "worktree",
                &args(&["list"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn worktree_prune_dry_run_is_passthrough() {
        assert_eq!(
            classify_pure(
                "worktree",
                &args(&["prune", "--dry-run"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn worktree_prune_without_dry_run_is_denied() {
        assert!(matches!(
            classify_pure(
                "worktree",
                &args(&["prune"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn checkout_to_protected_branch_is_denied() {
        let d = classify_pure(
            "checkout",
            &args(&["master"]),
            "master",
            &TrustRootTouch::Clean,
            false,
        );
        assert!(matches!(d, Disposition::Deny { .. }));
    }

    #[test]
    fn switch_to_feature_branch_passes_through() {
        assert_eq!(
            classify_pure(
                "switch",
                &args(&["feature/x"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn checkout_with_no_target_arg_passes_through() {
        // `git switch -` (previous branch) — nothing to protect against.
        assert_eq!(
            classify_pure(
                "switch",
                &args(&["-"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn push_touching_trust_root_on_feature_branch_passes_through() {
        // A PR-review gate still applies before this reaches the default
        // branch — same reasoning as any other feature-branch push.
        assert_eq!(
            classify_pure(
                "push",
                &args(&["origin", "feature/x"]),
                "master",
                &TrustRootTouch::Touched(vec!["Cargo.toml".to_string()]),
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn push_touching_trust_root_on_default_branch_is_denied() {
        assert!(matches!(
            classify_pure(
                "push",
                &args(&["origin", "master"]),
                "master",
                &TrustRootTouch::Touched(vec!["Cargo.toml".to_string()]),
                true
            ),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn push_not_touching_trust_root_passes_through() {
        assert_eq!(
            classify_pure(
                "push",
                &args(&["origin", "feature/x"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn push_of_default_branch_is_denied_even_without_trust_root_changes() {
        // Enforce PR-only: pushing the default branch straight to a remote is
        // blocked regardless of what the diff touches.
        assert!(matches!(
            classify_pure(
                "push",
                &args(&["origin", "master"]),
                "master",
                &TrustRootTouch::Clean,
                true
            ),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn push_of_feature_branch_is_not_a_default_branch_push() {
        assert_eq!(
            classify_pure(
                "push",
                &args(&["origin", "feature/x"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn branch_delete_of_protected_branch_is_denied() {
        assert!(matches!(
            classify_pure(
                "branch",
                &args(&["-D", "master"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Deny { .. }
        ));
        assert!(matches!(
            classify_pure(
                "branch",
                &args(&["--delete", "master"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn branch_rename_of_protected_branch_is_denied() {
        assert!(matches!(
            classify_pure(
                "branch",
                &args(&["-M", "master", "renamed"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn branch_delete_of_feature_branch_passes_through() {
        assert_eq!(
            classify_pure(
                "branch",
                &args(&["-D", "feature/x"]),
                "master",
                &TrustRootTouch::Clean,
                false
            ),
            Disposition::Passthrough
        );
    }

    #[test]
    fn branch_listing_and_creation_pass_through() {
        assert_eq!(
            classify_pure("branch", &[], "master", &TrustRootTouch::Clean, false),
            Disposition::Passthrough
        );
        assert_eq!(
            classify_pure(
                "branch",
                &args(&["feature/new"]),
                "master",
                &TrustRootTouch::Clean,
                false
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
    fn pushed_branch_reads_the_refspec_positionally_skipping_leading_flags() {
        // `-u` before remote/refspec previously threw off a fixed-index
        // `args[1]` read, misreading "origin" as the branch being pushed.
        let repo = crate::shell::test_support::init_repo_with_branch("master");
        assert_eq!(
            pushed_branch(&repo.path, &args(&["-u", "origin", "feature/x"])).as_deref(),
            Some("feature/x")
        );
        assert_eq!(
            pushed_branch(&repo.path, &args(&["--force", "origin", "feature/x"])).as_deref(),
            Some("feature/x")
        );
        assert_eq!(
            pushed_branch(&repo.path, &args(&["origin", "feature/x"])).as_deref(),
            Some("feature/x")
        );
    }

    #[test]
    fn pushed_branch_falls_back_to_current_branch_when_refspec_omitted() {
        // Bare `git push` and `git push <remote>` (no explicit ref) both push
        // the current/tracked branch -- previously these skipped the
        // trust-root check entirely (args.len() >= 2 was false).
        let repo = crate::shell::test_support::init_repo_with_branch("feature/y");
        assert_eq!(pushed_branch(&repo.path, &[]).as_deref(), Some("feature/y"));
        assert_eq!(
            pushed_branch(&repo.path, &args(&["origin"])).as_deref(),
            Some("feature/y")
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
        let repo = crate::shell::test_support::init_repo_with_branch("master");
        let event = classify(
            &repo.path,
            "worktree",
            &["add".to_string(), "../x".to_string()],
        );
        assert_eq!(event.disposition, Disposition::Passthrough);
    }

    #[test]
    fn protected_branch_checkout_passes_through_in_an_untracked_repo() {
        // The untracked-repo gate isn't worktree-specific: every deny this
        // policy produces exists for agentflare's own orchestration, so none
        // of it should apply outside a project agentflare actually tracks.
        let repo = crate::shell::test_support::init_repo_with_branch("master");
        let event = classify(&repo.path, "checkout", &["master".to_string()]);
        assert_eq!(event.disposition, Disposition::Passthrough);
    }

    #[test]
    fn protected_branch_checkout_is_still_denied_in_a_tracked_repo() {
        let repo = crate::shell::test_support::init_repo_with_branch("master");
        std::fs::create_dir_all(repo.path.join(".agentflare")).unwrap();
        let event = classify(&repo.path, "checkout", &["master".to_string()]);
        assert!(
            matches!(event.disposition, Disposition::Deny { .. }),
            "{:?}",
            event.disposition
        );
    }

    #[test]
    fn push_trust_root_deny_message_names_only_the_touched_path() {
        let touch = TrustRootTouch::Touched(vec!["Cargo.toml".to_string()]);
        let d = classify_pure("push", &args(&["origin", "master"]), "master", &touch, true);
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
        let d = classify_pure(
            "push",
            &args(&["origin", "master"]),
            "master",
            &TrustRootTouch::Unknown,
            true,
        );
        let Disposition::Deny { reason } = d else {
            panic!("expected Deny, got {d:?}");
        };
        assert!(reason.contains("could not be verified"), "{reason}");
    }

    #[test]
    fn push_with_unreadable_diff_on_feature_branch_passes_through() {
        assert_eq!(
            classify_pure(
                "push",
                &args(&["origin", "feature/x"]),
                "master",
                &TrustRootTouch::Unknown,
                false
            ),
            Disposition::Passthrough
        );
    }
}
