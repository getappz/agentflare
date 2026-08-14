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
use crate::policy_config::ResolvedGitShimPolicy;

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
/// change to and quietly weaken. CI workflow config is included for the same
/// reason as `.githooks/`/`.agentflare/`: an agent that can silently rewrite
/// the pipeline that's supposed to catch its own mistakes has defeated that
/// check before it ever runs (the "wipes CI" scenario named in the
/// competitor audit that led to this addition).
///
/// Known limitation (confirmed empirically 2026-07-29, see item #321):
/// `resolve_trust_root_touch` diffs the pushed branch against the default
/// branch by NAME — when both are literally "master" (the ordinary bare
/// `git push`/`git push origin master` case), that's a self-diff and always
/// resolves `Clean`, so a direct push of the default branch is denied by the
/// blanket "can't push the default branch" rule regardless of what changed,
/// never by this list. This entry only changes the deny *message* once a
/// pushed branch legitimately diverges under a different name (feature
/// branch, or after #321's src:dest refspec fix) — it doesn't add new
/// protection against the direct-push case, which was already fully blocked.
pub(crate) const TRUST_ROOT_PATHS: &[&str] = &[
    ".githooks/",
    ".agentflare/",
    "Cargo.toml",
    ".github/workflows/",
    ".gitlab-ci.yml",
    ".circleci/",
];

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

/// `true` if this invocation is (self-reportedly) agent-driven, not an
/// interactive human shell. Delegates to the `agent-detector` crate (already
/// a dependency here, see `provenance.rs`) rather than maintaining a second,
/// narrower hardcoded env-var list of the same kind `agentflare-shim` and
/// `flare-code::detect` each already carry -- `agent-detector` covers a much
/// wider agent catalog (opencode, codex, gemini, cursor, windsurf, aider,
/// devin, ...), not just the handful this crate used to check directly. The
/// `AGENTFLARE_AGENT` OR-clause stays as a second check alongside it: it's
/// agentflare's own internal marker (set by its orchestrator on subagents),
/// not something `agent-detector`'s external, tool-agnostic catalog knows
/// about, and `bypass_agent_env_var_bypasses_only_for_the_matching_agent`
/// (flare-git-shim's own tests) depends on it alone being sufficient to
/// count as agent-invoked.
#[must_use]
pub fn agent_invocation_detected() -> bool {
    agent_detector::is_agent()
        || std::env::var_os("AGENTFLARE_AGENT").is_some_and(|s| !s.is_empty())
}

/// `true` if `subcommand`/`args` is a branch-*creating* form: `git checkout
/// -b/-B/--orphan <name>` and `git switch -c/-C/--create/--force-create/
/// --orphan <name>` -- including the attached short-option spellings
/// (`-bname`, `-Cname`, ...) and `--long=name`. These never detach HEAD
/// (the new branch is checked out instead) but they DO move the canonical
/// checkout off its current branch onto feature-branch work -- so the shim
/// keeps blocking them there, with an accurate reason rather than the
/// misleading "would detach HEAD" message (item #441 / vent #395). Scanning
/// stops at a bare `--` -- anything after it is a pathspec, not an option.
#[must_use]
pub fn is_branch_create(subcommand: &str, args: &[String]) -> bool {
    let (short_flags, long_flags): (&[&str], &[&str]) = match subcommand {
        "checkout" => (&["-b", "-B"], &["--orphan"]),
        "switch" => (&["-c", "-C"], &["--create", "--force-create", "--orphan"]),
        _ => return false,
    };
    for a in args {
        if a == "--" {
            break;
        }
        let is_short = short_flags.iter().any(|f| a.starts_with(f));
        let is_long = long_flags
            .iter()
            .any(|f| a == f || a.starts_with(&format!("{f}=")));
        if is_short || is_long {
            return true;
        }
    }
    false
}

/// `true` if `subcommand`/`args` would detach HEAD -- `git checkout
/// <target>` implicitly detaches when `target` isn't an existing local
/// branch (no `--detach` flag required for that form); `git switch` never
/// silently detaches, only `switch --detach`/`-d` does. `git checkout --
/// <pathspec>` (and any form with `--` before the target) restores files
/// and never touches HEAD at all. Branch-creating forms (`-b`/`-B`/`-c`/
/// `-C`) check out the new branch and never detach -- handled by
/// `is_branch_create` instead.
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
            if is_branch_create(subcommand, args) {
                return false; // `checkout -b <name>` checks out the new branch
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
pub(crate) const ALLOWED_MUTATING_SUBCOMMANDS: &[&str] = &[
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
pub(crate) const DENIED_PLUMBING_SUBCOMMANDS: &[&str] = &[
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

/// Commits by which `target` and `HEAD` have diverged, when a `reset
/// --soft`/`--mixed` onto `target` would stage more than the caller's
/// intended change — i.e. neither ref is an ancestor of the other.
/// `--soft`/`--mixed` leave the working tree untouched and just move HEAD,
/// so the diff that ends up staged is `old_HEAD_tree` vs `target_tree`: when
/// `target` is a strict ancestor or descendant of HEAD (a clean fast-forward
/// either direction), that diff is exactly the commit(s) between them, which
/// is what the command is for. Once they've diverged, that same diff also
/// carries every change unique to `target`'s side — unrelated drift the
/// caller likely never intended to stage (item #98's live incident: `reset
/// --soft origin/master` from a stale branch staged a phantom crate deletion
/// that was actually a refactor on master, not a removal).
///
/// `None` if there's no divergence to warn about, or if it can't be
/// determined (unresolvable target, ...) — fails open, matching this
/// policy's bias toward never warning on something it can't actually reason
/// about.
fn reset_soft_divergence(repo_root: &Path, target: &str) -> Option<u32> {
    let head_is_ancestor =
        crate::shell::run_in_ok(repo_root, &["merge-base", "--is-ancestor", "HEAD", target]);
    let target_is_ancestor =
        crate::shell::run_in_ok(repo_root, &["merge-base", "--is-ancestor", target, "HEAD"]);
    if head_is_ancestor || target_is_ancestor {
        return None; // clean fast-forward in one direction or the other
    }
    let counts = crate::shell::run_in(
        repo_root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...{target}"),
        ],
    )
    .ok()?;
    let mut counts = counts.split_whitespace();
    let unique_to_head: u32 = counts.next()?.parse().ok()?;
    let unique_to_target: u32 = counts.next()?.parse().ok()?;
    Some(unique_to_head + unique_to_target)
}

/// `Some(warning)` if `subcommand`/`args` is a `reset --soft`/`--mixed`
/// targeting a ref that's diverged from HEAD (see `reset_soft_divergence`).
/// `--hard` is deliberately excluded — that's `is_destructive`'s job (a
/// working-tree-loss concern, already snapshotted before it runs), and
/// orthogonal to this one (a staged-diff surprise concern, which `--hard`
/// can't cause since it discards the index along with everything else).
#[must_use]
pub fn reset_soft_divergence_warning(
    repo_root: &Path,
    subcommand: &str,
    args: &[String],
) -> Option<String> {
    if subcommand != "reset" || !args.iter().any(|a| a == "--soft" || a == "--mixed") {
        return None;
    }
    let target = args
        .iter()
        .take_while(|a| a.as_str() != "--")
        .find(|a| !a.starts_with('-'))?;
    let commits = reset_soft_divergence(repo_root, target)?;
    Some(format!(
        "'{target}' and HEAD have diverged by {commits} commit(s) — `reset --soft`/`--mixed` will stage the full content diff between them, not just your intended change. Consider `git cherry-pick` onto a fresh branch instead."
    ))
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
    policy: &ResolvedGitShimPolicy,
) -> Disposition {
    if READ_ONLY_SUBCOMMANDS.contains(&subcommand)
        || policy
            .allowed_mutating_subcommands
            .iter()
            .any(|s| s.as_str() == subcommand)
    {
        return Disposition::Passthrough;
    }
    if policy
        .denied_plumbing_subcommands
        .iter()
        .any(|s| s.as_str() == subcommand)
    {
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
                        "'{target}' is this repo's default branch — direct checkout/switch is blocked by the agentflare git shim. Call `item(action=\"claim\", id=<item>)` to get an isolated worktree instead (not the standalone `claim`/`mcp__flare__claim` tool, which only takes a scope lock)."
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
                    "this push's diff against trust-root paths ({}) could not be verified, and it targets the repo's default branch '{default_branch}' — blocked by the agentflare git shim as a precaution.",
                    policy.trust_root_paths.join(", ")
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
                // Distinguish provisioning (`add`) from teardown (`remove`/
                // `prune`): an agent denied mid-teardown needs the exact
                // tool+action that owns cleanup, not the provisioning call.
                let teardown = matches!(
                    args.first().map(String::as_str),
                    Some("remove") | Some("prune")
                );
                let reason = if teardown {
                    "'git worktree remove/prune' is orchestrator-managed by agentflare — to tear down an item's worktree call `item(action=\"check_merge\", id=<item>)` once its PR merges, or `item(action=\"release\", id=<item>)`; to prune stale worktrees run `agentflare git worktree audit --prune`, or from an MCP-only session call `item(action=\"doctor\", reclaim=true)` (same scan/reclaim as `agentflare git doctor --reclaim`).".to_string()
                } else {
                    "'git worktree' is orchestrator-managed by agentflare — call `item(action=\"claim\", id=<item>)` to provision one. (Not the standalone `claim`/`mcp__flare__claim` tool -- that only takes a scope lock and does not create a worktree.)".to_string()
                };
                Disposition::Deny { reason }
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
pub fn resolve_trust_root_touch(
    repo_root: &Path,
    branch: &str,
    target: &str,
    trust_root_paths: &[String],
) -> TrustRootTouch {
    let range = format!("{target}...{branch}");
    match crate::shell::run_in(repo_root, &["diff", "--name-only", &range]) {
        Ok(names) => {
            let mut matched: Vec<String> = names
                .lines()
                .filter(|f| trust_root_paths.iter().any(|p| f.starts_with(p.as_str())))
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

/// One ref a `push` invocation would actually write, split into the two
/// halves classification needs separately: `source` is the local content
/// being pushed (what a trust-root diff runs against), `destination` is the
/// remote ref name being written to (what `is_protected_branch` gates on).
/// For a plain (no-colon) refspec, or the no-refspec fallback, the two are
/// the same branch name; a `src:dest` refspec is the one case they diverge.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PushRef {
    source: String,
    destination: String,
}

fn strip_ref_prefix(name: &str) -> String {
    name.strip_prefix("refs/heads/").unwrap_or(name).to_string()
}

/// Every local branch name, for `--all`/`--mirror` (which push every local
/// branch to its identically-named remote ref, not something namable from
/// `args` alone). `None` if the listing itself fails -- callers must treat
/// that as an unresolvable push, not as "no branches to worry about".
fn local_branches(repo_root: &Path) -> Option<Vec<String>> {
    crate::shell::run_in(
        repo_root,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads/"],
    )
    .ok()
    .map(|out| {
        out.lines()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .collect()
    })
}

/// One refspec argument (e.g. `feature/x`, `feature/x:master`, `:master` for
/// a remote-delete) into its source/destination pair. A bare name with no
/// colon pushes to a remote ref of the same name. An empty source half
/// (delete form) has no local content to diff -- `destination` doubles as
/// `source` so the trust-root diff below self-diffs to `Clean` (nothing was
/// pushed), while `is_protected_branch` on `destination` still catches
/// "this deletes the default branch remotely".
fn parse_refspec(spec: &str) -> PushRef {
    // A leading `+` force-marks the whole refspec (`+master`,
    // `+feature/x:master`) -- strip it before parsing. Left in place, the
    // no-colon case below carries it straight into `source`/`destination`
    // (`"+master"`), which then silently fails to match the default branch
    // name and lets a force push of the default branch slip the
    // direct-push guard entirely.
    let spec = spec.strip_prefix('+').unwrap_or(spec);
    match spec.split_once(':') {
        Some((src, dst)) if !dst.is_empty() => {
            let dst = strip_ref_prefix(dst);
            PushRef {
                source: if src.is_empty() {
                    dst.clone()
                } else {
                    strip_ref_prefix(src)
                },
                destination: dst,
            }
        }
        _ => {
            let name = strip_ref_prefix(spec.split(':').next().unwrap_or(spec));
            PushRef {
                source: name.clone(),
                destination: name,
            }
        }
    }
}

/// `git push` flags that consume a separate following argument (as opposed
/// to the `--flag=value` form, which already reads as one token starting
/// with `-` and is dropped whole by the flag filter below). Left
/// unhandled, the value token (e.g. the `myrepo.git` in `--repo myrepo.git
/// feature/x`) reads as an ordinary positional and throws off the
/// `non_flags[0]` = remote / `non_flags[1..]` = refspecs split -- in the
/// worst case (no explicit refspec) shifting a real branch name out of
/// position and silently swapping in the flag's value as the "branch"
/// being pushed, which then never matches the default branch name.
const VALUE_TAKING_PUSH_FLAGS: &[&str] =
    &["-o", "--push-option", "--receive-pack", "--repo", "--exec"];

/// Positional (non-flag) arguments, with the value token following any
/// `VALUE_TAKING_PUSH_FLAGS` entry dropped alongside the flag itself.
fn non_flag_args(args: &[String]) -> Vec<&str> {
    let mut out = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for a in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a.starts_with('-') {
            if VALUE_TAKING_PUSH_FLAGS.contains(&a.as_str()) {
                skip_next = true;
            }
            continue;
        }
        out.push(a.as_str());
    }
    out
}

/// Resolves every ref a `push` invocation would actually write, skipping
/// flags positionally (`-u`, `--force`, `--force-with-lease`, ...) rather
/// than assuming fixed indices -- a flag before the remote/refspec (e.g.
/// `git push -u origin feature/x`) previously threw off a fixed-index read,
/// misreading the remote name (`"origin"`) as the branch being pushed.
/// Models the push shapes agentflare's own audit found `pushed_branch`
/// collapsing into a single (wrong) branch name (item #321):
/// - an explicit `src:dest` refspec (or several) -- each pair kept separate,
///   so e.g. `git push origin feature/x:master` is judged on `master` (the
///   actual write target), not `feature/x` (what was previously read as the
///   pushed branch, silently bypassing default-branch protection).
/// - `--all` -- every local branch, each to its same-named remote ref, not
///   just the one branch a positional read would find.
/// - `--mirror` -- pushes every ref (branches, tags, and deletes anything
///   present remotely but not locally), a strictly larger surface than
///   `local_branches` enumerates. Approximating it as "every local branch"
///   could miss exactly the kind of write this exists to catch, so it's
///   treated as unresolvable rather than under-approximated.
/// - `--tags` with no explicit refspec -- pushes every tag, not a branch;
///   there's nothing here for branch-protection/trust-root checks to
///   inspect, so no ref (not even a current-branch fallback) applies.
/// - no refspec at all (bare `git push`, or `git push <remote>`) -- falls
///   back to the current checked-out branch, pushed to itself.
///
/// `None` means the push's targets couldn't be resolved at all (branch
/// enumeration failed, or there's no current branch to fall back to) --
/// callers must fail closed rather than assume nothing risky is being
/// pushed.
fn pushed_refs(repo_root: &Path, args: &[String]) -> Option<Vec<PushRef>> {
    if args.iter().any(|a| a == "--mirror") {
        return None;
    }
    if args.iter().any(|a| a == "--all") {
        return local_branches(repo_root).map(|branches| {
            branches
                .into_iter()
                .map(|b| PushRef {
                    source: b.clone(),
                    destination: b,
                })
                .collect()
        });
    }
    let non_flags = non_flag_args(args);
    if args.iter().any(|a| a == "--tags") && non_flags.len() <= 1 {
        return Some(Vec::new());
    }
    if non_flags.len() <= 1 {
        let branch = current_branch(repo_root)?;
        return Some(vec![PushRef {
            source: branch.clone(),
            destination: branch,
        }]);
    }
    Some(non_flags[1..].iter().map(|r| parse_refspec(r)).collect())
}

/// Folds `resolve_trust_root_touch` over every pushed ref's `source` --
/// `Touched` wins over `Unknown` wins over `Clean`, and matched paths union
/// across refs, so a trust-root change on ANY pushed ref is caught rather
/// than only the first/only one a single-branch read would have inspected.
fn combined_trust_root_touch(
    repo_root: &Path,
    refs: &[PushRef],
    default_branch: &str,
    trust_root_paths: &[String],
) -> TrustRootTouch {
    let mut touched: Vec<String> = Vec::new();
    let mut saw_unknown = false;
    for r in refs {
        match resolve_trust_root_touch(repo_root, &r.source, default_branch, trust_root_paths) {
            TrustRootTouch::Touched(paths) => touched.extend(paths),
            TrustRootTouch::Unknown => saw_unknown = true,
            TrustRootTouch::Clean => {}
        }
    }
    if !touched.is_empty() {
        touched.sort();
        touched.dedup();
        TrustRootTouch::Touched(touched)
    } else if saw_unknown {
        TrustRootTouch::Unknown
    } else {
        TrustRootTouch::Clean
    }
}

/// I/O-resolving entry point: resolves the default branch and (for `push`
/// with a resolvable branch/target pair) whether the push touches a
/// trust-root path, then delegates to `classify_pure`.
#[must_use]
pub fn classify(repo_root: &Path, subcommand: &str, args: &[String]) -> Event {
    classify_with_home(repo_root, subcommand, args, dirs::home_dir().as_deref())
}

/// Same as `classify`, but with the `in_scoped_project` home boundary
/// injectable -- the real ambient home dir's exact path (short-name vs.
/// long-name forms, drive/case differences) isn't something a test should
/// depend on to stay deterministic across platforms/CI runners; a synthetic
/// `home` here mirrors how `agentflare_shim::in_scoped_project`'s own tests
/// already avoid that dependency.
#[must_use]
pub fn classify_with_home(
    repo_root: &Path,
    subcommand: &str,
    args: &[String],
    home: Option<&Path>,
) -> Event {
    let policy = crate::policy_config::resolve(repo_root, home).unwrap_or_else(|e| {
        eprintln!(
            "WARNING: agentflare git-shim config at {} is invalid ({}) -- \
             using baseline policy only, no config-sourced additions applied. \
             Git operations are not blocked by this; fix the file to restore \
             your customizations.",
            e.path.display(),
            e.source
        );
        ResolvedGitShimPolicy::baseline()
    });
    let default_branch = resolve_default_branch(repo_root);
    // Resolve every ref the push actually writes, then derive both push
    // facts from the full set: whether ANY destination *is* the default
    // branch (direct push blocked in favour of a PR), and -- only then --
    // whether ANY of them carries trust-root changes. `classify_pure`
    // never looks at `trust_root_touch` unless `targets_default_branch` is
    // true (every match arm below it is gated on that), so checking it
    // first skips `combined_trust_root_touch`'s `git diff` subprocesses
    // entirely for the common case of a push that isn't touching the
    // default branch. An unresolvable push mode/refspec (`pushed_refs`
    // returning `None`) fails closed -- forcing `targets_default_branch`
    // true regardless of `trust_root_touch` is what actually makes that
    // closed: `classify_pure`'s `Unknown` arm only denies when paired with
    // `targets_default_branch`, so leaving it `false` here would let an
    // unresolvable push straight through.
    let (trust_root_touch, targets_default_branch) = if subcommand == "push" {
        match pushed_refs(repo_root, args) {
            Some(refs) => {
                let targets_default_branch = refs
                    .iter()
                    .any(|r| is_protected_branch(&r.destination, Some(&default_branch)));
                let trust_root_touch = if targets_default_branch {
                    combined_trust_root_touch(
                        repo_root,
                        &refs,
                        &default_branch,
                        &policy.trust_root_paths,
                    )
                } else {
                    TrustRootTouch::Clean
                };
                (trust_root_touch, targets_default_branch)
            }
            None => (TrustRootTouch::Unknown, true),
        }
    } else {
        (TrustRootTouch::Clean, false)
    };
    let mut disposition = classify_pure(
        subcommand,
        args,
        &default_branch,
        &trust_root_touch,
        targets_default_branch,
        &policy,
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
        && !agentflare_shim::in_scoped_project(repo_root, home)
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
        let policy = ResolvedGitShimPolicy::baseline();
        assert_eq!(
            classify_pure(
                "status",
                &[],
                "master",
                &TrustRootTouch::Clean,
                false,
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
                &policy,
            );
            let Disposition::Deny { reason } = d else {
                panic!("expected deny for worktree {sub}");
            };
            assert!(reason.contains("check_merge"), "{reason}");
            assert!(reason.contains("audit --prune"), "{reason}");
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
                &policy
            ),
            Disposition::Deny { .. }
        ));
    }

    #[test]
    fn checkout_to_protected_branch_is_denied() {
        let policy = ResolvedGitShimPolicy::baseline();
        let d = classify_pure(
            "checkout",
            &args(&["master"]),
            "master",
            &TrustRootTouch::Clean,
            false,
            &policy,
        );
        assert!(matches!(d, Disposition::Deny { .. }));
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

        let msg =
            reset_soft_divergence_warning(&repo.path, "reset", &args(&["--mixed", "feature"]));
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
        let event = classify(&repo.path, "checkout", &["master".to_string()]);
        assert!(
            matches!(event.disposition, Disposition::Deny { .. }),
            "{:?}",
            event.disposition
        );
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
}
