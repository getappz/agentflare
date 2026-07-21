// PreToolUse redirect classifier — nudges the agent toward agentflare-backend's
// own tools instead of ad-hoc file-based tracking. Flow ported from lean-ctx's
// hook_handlers (classify -> fail-open timeout -> dual JSON decision): a
// synchronous classify step run under a hard wall-clock budget, so a future
// redirect rule that needs IO (e.g. a backend DB lookup) can never wedge the
// host's tool call — it just falls through to allow instead.
use serde_json::{Value, json};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Hard wall-clock budget for classify_and_decide. Sized well under the 5s
/// timeout `init.rs` wires into `~/.claude/settings.json`'s PreToolUse entry,
/// so a hang here can never eat the whole hook budget.
const GATING_TIMEOUT: Duration = Duration::from_millis(2000);

/// Native/MCP tools that mutate files on disk — every one of these is gated
/// by `branch_guard_reason` so a direct edit can never land on the repo's
/// default branch, regardless of which of these the agent reaches for.
/// Includes opencode's native tool names (`write`/`edit` lowercase already
/// covered Claude Code's own lowercase variants; `patch`/`apply_patch`/
/// `multiedit` are opencode-specific) — the opencode branch-guard plugin
/// (`~/.config/opencode/plugin/branch-guard.js`) calls this same classifier
/// via `agentflare hook pre-tool-use` instead of duplicating branch logic.
const MUTATING_TOOLS: &[&str] = &[
    "Write",
    "write",
    "Edit",
    "edit",
    "NotebookEdit",
    "notebookedit",
    "MultiEdit",
    "multiedit",
    "patch",
    "apply_patch",
    "mcp__lean-ctx__ctx_patch",
    "mcp__lean-ctx__ctx_edit",
];

/// Run `work` under a hard timeout, returning `None` (allow-passthrough) if
/// it doesn't finish in time. `work` only sends to a channel, never prints,
/// so a timed-out worker can't double-write stdout once it eventually
/// finishes.
fn decide_with_timeout<F>(timeout: Duration, work: F) -> Option<Value>
where
    F: FnOnce() -> Option<Value> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(timeout).unwrap_or(None)
}

fn is_spec_like_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.contains("/specs/") && normalized.ends_with(".md")
}

/// Resolve the current branch of the repo containing `start_path`, or cwd if
/// `start_path` is None. `None` outside a git repo.
fn current_branch(start_path: Option<&Path>) -> Option<String> {
    if let Some(p) = start_path {
        flare_git_core::branch::current_branch(p)
    } else {
        flare_git_core::branch::current_branch(&std::env::current_dir().ok()?)
    }
}

/// Resolve the default branch of the repo containing `start_path`, or cwd if
/// `start_path` is None.
fn default_branch(start_path: Option<&Path>) -> Option<String> {
    if let Some(p) = start_path {
        Some(flare_git_core::branch::resolve_default_branch(p))
    } else {
        Some(flare_git_core::branch::resolve_default_branch(
            &std::env::current_dir().ok()?,
        ))
    }
}

/// Pure decision core for the branch guard — no git process spawned here, so
/// it's unit-testable with fake branch names regardless of which branch this
/// actual repo happens to be on when `cargo test` runs (same reason
/// `AgentflareMcp` carries a `worktree_repo_root_override` for its own git
/// operations). `branch` is `None` outside a git repo (git missing, not a
/// repo) — never blocked, since "on the default branch" doesn't apply.
fn branch_guard_reason_for(branch: Option<&str>, default: Option<&str>) -> Option<String> {
    let branch = branch?;
    let is_protected = match default {
        Some(default) => branch == default,
        // Resolution failed entirely (no git, no remote, no main/master
        // branch found) — fall back to guessing against the two
        // conventional names instead of comparing against nothing.
        None => branch == "main" || branch == "master",
    };
    is_protected.then(|| {
        format!(
            "'{branch}' is this repo's default branch — direct edits are blocked. Create an isolated worktree first (e.g. `git worktree add ../<dir> -b <branch-name>`) and retry the edit there; a plain `git checkout -b <branch-name>` works too if a full worktree isn't needed."
        )
    })
}

/// Classify one PreToolUse payload into a redirect reason, if any. Returns
/// `None` for every tool call that isn't one of agentflare's own redirect
/// targets. `branch_ctx` carries the (current, default) branch pair so tests
/// can inject fake git state instead of depending on this repo's real branch.
fn classify(
    tool_name: &str,
    tool_input: Option<&Value>,
    branch_ctx: (Option<&str>, Option<&str>),
) -> Option<String> {
    if MUTATING_TOOLS.contains(&tool_name)
        && let Some(reason) = branch_guard_reason_for(branch_ctx.0, branch_ctx.1)
    {
        return Some(reason);
    }
    match tool_name {
        "TodoWrite" => Some(
            "agentflare-backend's item tracker is wired up for this repo — use the `item` MCP tool (action=create) instead of TodoWrite for anything that should survive past this session.".to_string(),
        ),
        "Write" | "Edit" => {
            let path = tool_input
                .and_then(|v| v.get("file_path").or_else(|| v.get("path")))
                .and_then(Value::as_str)?;
            is_spec_like_path(path).then(|| {
                format!(
                    "specs/design docs/plans belong attached to the relevant item as an asset (the `asset` tool, action=attach), not committed to the repo at '{path}' — create/assign an item first if one doesn't already track this work."
                )
            })
        }
        _ => None,
    }
}

/// Build the PreToolUse deny decision for a classified redirect, or `None` to
/// let the call through unchanged. Resolves the target file's git repo for
/// branch guard checks (not host cwd), so editing a file outside any git repo
/// (e.g. ~/.claude/memory/) is never blocked, and editing a file in a
/// different repo than cwd checks that repo's branch, not cwd's.
pub fn redirect_decision(tool_name: &str, tool_input: Option<&Value>) -> Option<Value> {
    let tool_name = tool_name.to_string();
    let tool_input = tool_input.cloned();
    decide_with_timeout(GATING_TIMEOUT, move || {
        // Only mutating tools ever consult the branch guard — resolving it
        // unconditionally would spawn several git subprocesses on every
        // single tool call (Read, Bash, Grep, ...), not just the ones that
        // need it. When we do check, resolve the target file's repo, not
        // host cwd.
        let (current, default) = if MUTATING_TOOLS.contains(&tool_name.as_str()) {
            let target_path = tool_input.as_ref().and_then(|ti| {
                ti.get("file_path")
                    .or_else(|| ti.get("path"))
                    .and_then(Value::as_str)
                    .map(Path::new)
            });
            // Walk up from the target to the first ancestor that actually
            // exists on disk before asking git for its toplevel -- a bare
            // filename's parent is "" (no such dir) and a new file's parent
            // may not exist yet, either of which would otherwise make the
            // git subprocess fail and silently skip the guard.
            // `git rev-parse --show-toplevel` already walks up from its
            // start dir looking for `.git`, so only the FIRST existing
            // ancestor needs to actually be handed to it -- every higher
            // ancestor is already covered by that walk, and re-spawning git
            // per ancestor just burns time against GATING_TIMEOUT.
            let target_repo = target_path.and_then(|p| {
                let first_existing = p.ancestors().skip(1).find(|ancestor| {
                    let check = if *ancestor == Path::new("") {
                        Path::new(".")
                    } else {
                        *ancestor
                    };
                    check.exists()
                })?;
                let check = if first_existing == Path::new("") {
                    Path::new(".")
                } else {
                    first_existing
                };
                flare_git_core::branch::repo_toplevel(check)
            });
            match (target_path, target_repo) {
                // Path was extracted but isn't in any git repo -- no guard.
                (Some(_), None) => (None, None),
                // Path couldn't be extracted (tool has no file_path/path,
                // e.g. MultiEdit) -- fall back to cwd; repo found -- use it.
                (_, repo) => (
                    current_branch(repo.as_deref()),
                    default_branch(repo.as_deref()),
                ),
            }
        } else {
            (None, None)
        };
        let reason = classify(
            &tool_name,
            tool_input.as_ref(),
            (current.as_deref(), default.as_deref()),
        )?;
        Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOT_A_REPO: (Option<&str>, Option<&str>) = (None, None);
    const ON_FEATURE_BRANCH: (Option<&str>, Option<&str>) = (Some("feature/x"), Some("main"));

    #[test]
    fn classify_redirects_todo_write() {
        let reason = classify("TodoWrite", None, NOT_A_REPO).unwrap();
        assert!(reason.contains("`item` MCP tool"));
    }

    #[test]
    fn classify_redirects_spec_path_write() {
        let input = json!({ "file_path": "docs/superpowers/specs/2026-07-13-foo.md" });
        let reason = classify("Write", Some(&input), ON_FEATURE_BRANCH).unwrap();
        assert!(reason.contains("`asset` tool"));
    }

    #[test]
    fn classify_redirects_spec_path_edit_on_windows_backslashes() {
        let input = json!({ "file_path": "docs\\superpowers\\specs\\foo.md" });
        assert!(classify("Edit", Some(&input), ON_FEATURE_BRANCH).is_some());
    }

    #[test]
    fn classify_ignores_non_spec_write() {
        let input = json!({ "file_path": "src/main.rs" });
        assert!(classify("Write", Some(&input), ON_FEATURE_BRANCH).is_none());
    }

    #[test]
    fn classify_ignores_unrelated_tools() {
        assert!(classify("Read", None, NOT_A_REPO).is_none());
        assert!(classify("Bash", None, NOT_A_REPO).is_none());
    }

    #[test]
    fn classify_write_with_no_path_falls_through() {
        assert!(classify("Write", None, ON_FEATURE_BRANCH).is_none());
        assert!(classify("Write", Some(&json!({})), ON_FEATURE_BRANCH).is_none());
    }

    #[test]
    fn classify_blocks_write_on_default_branch_named_master() {
        let reason = classify(
            "Write",
            Some(&json!({"file_path": "src/main.rs"})),
            (Some("master"), None),
        )
        .unwrap();
        assert!(reason.contains("worktree"), "{reason}");
        assert!(reason.contains("default branch"), "{reason}");
    }

    #[test]
    fn classify_blocks_edit_on_resolved_default_branch_name() {
        let ctx = (Some("trunk"), Some("trunk"));
        let reason = classify("Edit", Some(&json!({"file_path": "src/main.rs"})), ctx).unwrap();
        assert!(reason.contains("'trunk'"), "{reason}");
    }

    #[test]
    fn classify_blocks_notebook_edit_and_ctx_patch_and_ctx_edit_on_master() {
        let ctx = (Some("master"), None);
        assert!(classify("NotebookEdit", None, ctx).is_some());
        assert!(classify("mcp__lean-ctx__ctx_patch", None, ctx).is_some());
        assert!(classify("mcp__lean-ctx__ctx_edit", None, ctx).is_some());
    }

    #[test]
    fn classify_blocks_lowercase_edit_and_write_on_master() {
        let ctx = (Some("master"), None);
        assert!(classify("edit", None, ctx).is_some());
        assert!(classify("write", None, ctx).is_some());
        assert!(classify("notebookedit", None, ctx).is_some());
    }

    #[test]
    fn classify_blocks_opencode_native_tool_names_on_master() {
        let ctx = (Some("master"), None);
        assert!(classify("patch", None, ctx).is_some());
        assert!(classify("apply_patch", None, ctx).is_some());
        assert!(classify("multiedit", None, ctx).is_some());
        assert!(classify("MultiEdit", None, ctx).is_some());
    }

    #[test]
    fn classify_allows_mutating_tools_on_a_feature_branch() {
        assert!(classify("NotebookEdit", None, ON_FEATURE_BRANCH).is_none());
        assert!(classify("mcp__lean-ctx__ctx_patch", None, ON_FEATURE_BRANCH).is_none());
    }

    #[test]
    fn classify_allows_writes_outside_any_git_repo() {
        let input = json!({ "file_path": "src/main.rs" });
        assert!(classify("Write", Some(&input), NOT_A_REPO).is_none());
    }

    #[test]
    fn branch_guard_reason_for_prefers_default_over_hardcoded_names() {
        // A repo whose default branch is deliberately named neither
        // main nor master must still be caught via the resolved default.
        assert!(branch_guard_reason_for(Some("develop"), Some("develop")).is_some());
        assert!(branch_guard_reason_for(Some("feature/y"), Some("develop")).is_none());
    }

    #[test]
    fn redirect_decision_builds_deny_shape_for_todo_write() {
        let decision = redirect_decision("TodoWrite", None).unwrap();
        assert_eq!(
            decision["hookSpecificOutput"]["hookEventName"],
            "PreToolUse"
        );
        assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(
            decision["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("item")
        );
    }

    #[test]
    fn redirect_decision_is_none_for_unmatched_tool() {
        assert!(redirect_decision("Grep", None).is_none());
    }

    #[test]
    fn decide_with_timeout_fails_open_on_slow_work() {
        let out = decide_with_timeout(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(500));
            Some(json!({ "should": "never observe this" }))
        });
        assert!(
            out.is_none(),
            "a worker slower than the timeout must fail open to None"
        );
    }

    /// `git init` a temp repo with one commit on `branch` -- enough to
    /// exercise `redirect_decision`'s real git subprocess path
    /// (`test_support` in flare-git-core is `pub(crate)`, so this binary
    /// crate can't reuse it). Every path handed to `redirect_decision` in
    /// these tests is absolute (anchored at the returned repo's own path),
    /// so none of them need to touch the real process cwd -- mutating that
    /// is global, process-wide state that a parallel test binary can't
    /// safely share (a prior version of this test file did exactly that
    /// and intermittently broke unrelated cwd-sensitive tests elsewhere in
    /// the same binary).
    fn init_temp_repo(branch: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q", "-b", branch]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "test"]);
        std::fs::write(dir.path().join("seed.txt"), "seed").unwrap();
        run(&["add", "seed.txt"]);
        run(&["commit", "-q", "-m", "seed"]);
        dir
    }

    #[test]
    fn redirect_decision_guards_new_nested_path_via_ancestor_walk() {
        // Regression for the CodeRabbit-flagged bypass on PR #283: a new
        // file under a directory that doesn't exist yet used to make the
        // git subprocess fail (parent dir ENOENT) and silently skip the
        // guard. The path is absolute (anchored at the temp repo), so this
        // doesn't depend on the real process cwd at all.
        let repo = init_temp_repo("master");
        let target = repo.path().join("new_dir").join("does_not_exist_yet.txt");
        let decision = redirect_decision(
            "Write",
            Some(&json!({"file_path": target.to_str().unwrap()})),
        );
        assert!(
            decision.is_some(),
            "a new file under a not-yet-created directory must still be guarded"
        );
    }

    #[test]
    fn redirect_decision_bare_filename_matches_explicit_cwd_fallback() {
        // Second bypass: a bare filename's `.parent()` is `""`, which used
        // to be handed straight to `repo_toplevel` (ENOENT -> None -> guard
        // silently skipped) regardless of what repo the agent was actually
        // in. Rather than mutating the real process cwd (unsafe to do in a
        // parallel test binary -- see `init_temp_repo`'s doc comment), this
        // proves the fix by asserting the bare-filename path now resolves
        // to the SAME outcome as the already-supported explicit-`None`
        // cwd-fallback path, whatever repo/branch this test happens to run
        // in.
        let expected = redirect_decision("MultiEdit", Some(&json!({"edits": []})));
        let actual = redirect_decision("Write", Some(&json!({"file_path": "bare_filename.txt"})));
        assert_eq!(actual.is_some(), expected.is_some());
        assert_eq!(actual, expected);
    }

    #[test]
    fn redirect_decision_missing_path_field_falls_back_to_cwd() {
        // Third bypass: MultiEdit-shaped input has no top-level file_path,
        // which used to make target_repo resolution bail out to
        // `(None, None)` unconditionally instead of falling back to cwd.
        // Ground truth here is computed directly from `flare_git_core`
        // against `Path::new(".")` rather than a hardcoded branch name, so
        // this holds regardless of what repo/branch actually checks out
        // this crate's tests.
        let expected_current = flare_git_core::branch::current_branch(Path::new("."));
        let expected_default = Some(flare_git_core::branch::resolve_default_branch(Path::new(
            ".",
        )));
        let expected_reason =
            branch_guard_reason_for(expected_current.as_deref(), expected_default.as_deref());

        let decision = redirect_decision("MultiEdit", Some(&json!({"edits": []})));
        assert_eq!(decision.is_some(), expected_reason.is_some());
        if let Some(reason) = expected_reason {
            assert_eq!(
                decision.unwrap()["hookSpecificOutput"]["permissionDecisionReason"],
                reason
            );
        }
    }

    #[test]
    fn redirect_decision_still_skips_guard_outside_any_repo() {
        // Not a regression case, but pins down the intended non-bypass
        // behavior: a target genuinely outside any git repo must still
        // pass through unguarded, ancestor walk or not.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("file.txt");
        let decision = redirect_decision(
            "Write",
            Some(&json!({"file_path": target.to_str().unwrap()})),
        );
        assert!(decision.is_none());
    }
}
