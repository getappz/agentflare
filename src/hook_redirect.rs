// PreToolUse redirect classifier — nudges the agent toward agentflare-backend's
// own tools instead of ad-hoc file-based tracking. Flow ported from lean-ctx's
// hook_handlers (classify -> fail-open timeout -> dual JSON decision): a
// synchronous classify step run under a hard wall-clock budget, so a future
// redirect rule that needs IO (e.g. a backend DB lookup) can never wedge the
// host's tool call — it just falls through to allow instead.
use serde_json::{Value, json};
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
const MUTATING_TOOLS: &[&str] = &[
    "Write",
    "write",
    "Edit",
    "edit",
    "NotebookEdit",
    "notebookedit",
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

fn current_branch() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    crate::git::current_branch(&cwd)
}

fn default_branch() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    Some(crate::git::resolve_default_branch(&cwd))
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
/// let the call through unchanged.
pub fn redirect_decision(tool_name: &str, tool_input: Option<&Value>) -> Option<Value> {
    let tool_name = tool_name.to_string();
    let tool_input = tool_input.cloned();
    decide_with_timeout(GATING_TIMEOUT, move || {
        // Only mutating tools ever consult the branch guard (see
        // `classify`) — resolving it unconditionally would spawn several
        // git subprocesses on every single tool call (Read, Bash, Grep,
        // ...), not just the handful that actually need it.
        let (current, default) = if MUTATING_TOOLS.contains(&tool_name.as_str()) {
            (current_branch(), default_branch())
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
}
