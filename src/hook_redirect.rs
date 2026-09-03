// PreToolUse redirect classifier — nudges the agent toward agentflare-backend's
// own tools instead of ad-hoc file-based tracking. Flow ported from lean-ctx's
// hook_handlers (classify -> fail-open timeout -> dual JSON decision): a
// synchronous classify step run under a hard wall-clock budget, so a future
// redirect rule that needs IO (e.g. a backend DB lookup) can never wedge the
// host's tool call — it just falls through to allow instead.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Hard wall-clock budget for classify_and_decide. Sized well under the 5s
/// timeout `init.rs` wires into `~/.claude/settings.json`'s PreToolUse entry,
/// so a hang here can never eat the whole hook budget.
const GATING_TIMEOUT: Duration = Duration::from_millis(2000);

/// Native/MCP tools that mutate files on disk — every one of these is gated
/// by `branch_guard_reason_for` so a direct edit can never land on the repo's
/// default branch, regardless of which of these the agent reaches for.
/// Includes opencode's native tool names (`write`/`edit` lowercase already
/// covered Claude Code's own lowercase variants; `patch`/`apply_patch`/
/// `multiedit` are opencode-specific) — the opencode branch-guard plugin
/// (`~/.config/opencode/plugin/branch-guard.js`) calls this same classifier
/// via `agentflare hook pre-tool-use` instead of duplicating branch logic.
pub(crate) const MUTATING_TOOLS: &[&str] = &[
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

/// Blocks shell commands that delete agentflare's own SQLite data files
/// (`~/.agentflare/*.db*`, or the `.agentflare` dir wholesale). Landed after
/// a 2026-07-25 incident: opencode ran a delete mid-migration and silently
/// wiped `store.db`'s metadata for 168 artifacts — recovered by hand from a
/// pre-migration flat-file backup that happened to still exist, but nothing
/// would have caught it if that backup hadn't been there. `rm`/`del`/
/// `Remove-Item`/`unlink`/`rmdir` are the verbs every shell tool (Bash,
/// PowerShell, opencode's `bash`) actually uses; deletion of these files is
/// something only the user should ever do by hand.
///
/// Checks each `;`/`&&`/`||`/`|`/newline-separated statement's *first word*
/// against the destructive-verb list, rather than substring-matching the
/// whole command blob — a raw `.contains("rm ")` also fires on a `git commit`
/// whose heredoc message happens to describe this very guard in prose (this
/// function's own commit message is a real example: "opencode ran an rm
/// mid-migration ... store.db ... ~/.agentflare/*.db*" — none of that is an
/// executed command, but a whole-string substring check can't tell).
fn destructive_data_file_reason(command: &str) -> Option<String> {
    for statement in command
        .split([';', '\n'])
        .flat_map(|s| s.split("&&"))
        .flat_map(|s| s.split("||"))
        .flat_map(|s| s.split('|'))
    {
        let trimmed = statement.trim().to_lowercase().replace('\\', "/");
        let Some(first_word) = trimmed.split_whitespace().next() else {
            continue;
        };
        let is_destructive_verb = matches!(
            first_word,
            "rm" | "del" | "erase" | "remove-item" | "unlink" | "rmdir"
        );
        if !is_destructive_verb {
            continue;
        }
        let targets_agentflare_dir =
            trimmed.contains(".agentflare/") || trimmed.ends_with(".agentflare");
        if !targets_agentflare_dir {
            continue;
        }
        // Either a specific *.db*/-wal/-shm file, or a recursive/whole-dir
        // delete of .agentflare itself (which would take the db files with it).
        let targets_db_or_whole_dir = trimmed.contains(".db")
            || trimmed.ends_with(".agentflare")
            || trimmed.contains(" -r ")
            || trimmed.contains(" -rf")
            || trimmed.contains("-recurse");
        if targets_db_or_whole_dir {
            return Some("deleting agentflare's local data files (~/.agentflare/*.db, *.db-wal, *.db-shm, or the .agentflare directory itself) is blocked — they hold tracked items, artifacts, and secrets with no automatic backup. If a file genuinely needs to be removed, ask the user to run the command themselves.".to_string());
        }
    }
    None
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

/// Where a mutating tool's edit will actually land, for any guard that
/// needs "target isn't in a repo at all" to differ from "tool gave no path,
/// fall back to cwd" -- conflating the two is exactly the AGENTFLARE-6 bug
/// (a file's own repo/branch silently swapped for the host cwd's).
pub(crate) enum TargetRepo {
    /// Repo resolved -- from the tool's target path, or cwd when the tool
    /// gave no path at all (e.g. MultiEdit).
    Found(PathBuf),
    /// Tool gave an explicit path that isn't inside ANY git repo -- callers
    /// must not fall back to cwd's repo/branch instead.
    Outside,
}

/// Resolves the repo a mutating tool's edit targets. Walks up from the
/// target path to the first ancestor that actually exists on disk before
/// asking git for its toplevel -- a bare filename's parent is "" (no such
/// dir) and a new file's parent may not exist yet, either of which would
/// otherwise make the git subprocess fail and silently skip whichever guard
/// calls this. `git rev-parse --show-toplevel` already walks up from its
/// start dir looking for `.git`, so only the FIRST existing ancestor needs
/// to actually be handed to it.
pub(crate) fn resolve_mutating_target_repo(tool_input: Option<&Value>) -> TargetRepo {
    let target_path = tool_input.and_then(|ti| {
        // opencode's native tools send camelCase `filePath`.
        ti.get("file_path")
            .or_else(|| ti.get("path"))
            .or_else(|| ti.get("filePath"))
            .and_then(Value::as_str)
            .map(Path::new)
    });
    let Some(p) = target_path else {
        let Ok(cwd) = std::env::current_dir() else {
            return TargetRepo::Outside;
        };
        return match flare_git_core::branch::repo_toplevel(&cwd) {
            Some(repo) => TargetRepo::Found(repo),
            None => TargetRepo::Outside,
        };
    };
    let Some(first_existing) = p.ancestors().skip(1).find(|ancestor| {
        let check = if *ancestor == Path::new("") {
            Path::new(".")
        } else {
            *ancestor
        };
        check.exists()
    }) else {
        return TargetRepo::Outside;
    };
    let check = if first_existing == Path::new("") {
        Path::new(".")
    } else {
        first_existing
    };
    match flare_git_core::branch::repo_toplevel(check) {
        Some(repo) => TargetRepo::Found(repo),
        None => TargetRepo::Outside,
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

/// MCP tool name Claude Code sends for the flare gateway's own `tool` MCP
/// tool. `apply_gateway_permissions` (components.rs) steers agents toward
/// calling lean-ctx exclusively through this gateway (`action="execute"`)
/// and actively strips their direct `mcp__lean-ctx__*` permissions -- so
/// once that's applied, every `ctx_shell`/`ctx_patch`/`ctx_edit` call this
/// module or `hook_completion_gate` needs to classify arrives wrapped in
/// this envelope instead of at the top level.
const GATEWAY_TOOL_NAME: &str = "mcp__flare__tool";

/// Unwraps a flare-gateway `action="execute"` call (`mcp__flare__tool(
/// action="execute", server="leanctx", tool="ctx_shell", args={"command":
/// "..."})`) to the real tool name and arguments it forwards, so
/// `is_verification_command`, `MUTATING_TOOLS`, and
/// `destructive_data_file_reason` all see the actual command/tool instead of
/// the gateway envelope one level up. Every other call (non-gateway, or
/// gateway `action="search"`, or `server` other than `"leanctx"`) passes
/// through with `tool_name`/`tool_input` unchanged.
///
/// Root cause of item #559: without this, a verification command (or a
/// `ctx_patch`/`ctx_edit` edit) run through the gateway was invisible to
/// every classifier in this module -- `is_verification_command` never saw
/// the real command, so the completion gate (`completion_gate_reason`)
/// rejected `item done`/`check_merge` even seconds after a passing test run,
/// on every single retry, once gateway routing was in effect.
pub(crate) fn unwrap_gateway_call(
    tool_name: &str,
    tool_input: Option<&Value>,
) -> (String, Option<Value>) {
    let passthrough = || (tool_name.to_string(), tool_input.cloned());
    if tool_name != GATEWAY_TOOL_NAME {
        return passthrough();
    }
    let Some(input) = tool_input else {
        return passthrough();
    };
    if input.get("action").and_then(Value::as_str) != Some("execute") {
        return passthrough();
    }
    if input.get("server").and_then(Value::as_str) != Some("leanctx") {
        return passthrough();
    }
    let Some(real_tool) = input.get("tool").and_then(Value::as_str) else {
        return passthrough();
    };
    let args = input.get("args").cloned().unwrap_or_else(|| json!({}));
    (format!("mcp__lean-ctx__{real_tool}"), Some(args))
}

/// MCP tool name Claude Code sends for the `flare` server's `item` tool
/// (`mcp__<server>__<tool>`). opencode's MCP bridge is expected to use the
/// same convention -- if a harness turns out to send a bare `item` instead,
/// [`completion_gate_reason`] below matches that too.
pub(crate) const ITEM_TOOL_NAME: &str = "mcp__flare__item";

/// `item` actions that hand implementation off as finished -- `done` opens/
/// updates a PR and moves the item to in_review; `check_merge` promotes
/// in_review -> completed once that PR is confirmed merged. Both are the
/// completion-claim moment the verification gate protects (item #169).
const GATED_ITEM_ACTIONS: &[&str] = &["done", "check_merge"];

/// Blocks `item done` / `item check_merge` until this session has a fresh,
/// passing verification-evidence record (`crate::optimize::VerificationEvidence`,
/// captured by the `PostToolUse` success hook when a test/build/lint command
/// runs). Closes the `verification-before-completion` gap from item #168's
/// gap analysis: nothing previously stopped an agent from claiming
/// "done"/opening a PR without having actually run tests *now* -- "tests
/// passed earlier this session" doesn't count once the evidence goes stale
/// (see `VERIFICATION_FRESHNESS_SECS`).
pub(crate) fn completion_gate_reason(
    tool_name: &str,
    tool_input: Option<&Value>,
    has_fresh_verification: bool,
) -> Option<String> {
    if tool_name != ITEM_TOOL_NAME && tool_name != "item" {
        return None;
    }
    let action = tool_input?.get("action")?.as_str()?;
    if !GATED_ITEM_ACTIONS.contains(&action) {
        return None;
    }
    if has_fresh_verification {
        return None;
    }
    Some(format!(
        "no fresh, passing verification evidence for this session -- run this project's test/build/lint command (e.g. `cargo test`, `npm test`, `pytest`) via Bash before calling `item` action={action}; a run more than {}m ago, or one that failed, doesn't count.",
        crate::optimize::VERIFICATION_FRESHNESS_SECS / 60
    ))
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
                .and_then(|v| {
                    // opencode's native tools send camelCase `filePath`.
                    v.get("file_path")
                        .or_else(|| v.get("path"))
                        .or_else(|| v.get("filePath"))
                })
                .and_then(Value::as_str)?;
            is_spec_like_path(path).then(|| {
                format!(
                    "specs/design docs/plans belong attached to the relevant item as an asset (the `asset` tool, action=attach), not committed to the repo at '{path}' — create/assign an item first if one doesn't already track this work."
                )
            })
        }
        "Bash" | "bash" | "PowerShell" | "powershell" | "shell" | "mcp__lean-ctx__ctx_shell" => {
            // Includes ctx_shell (direct or gateway-unwrapped, see
            // `unwrap_gateway_call`) -- item #559: a destructive `rm
            // ~/.agentflare/*.db` run via ctx_shell must be caught exactly
            // like the same command run via Bash.
            //
            // "command" is Claude Code's and (by convention) opencode's bash
            // tool field; "cmd"/"script" are cheap insurance against a
            // harness using a different name rather than a hard dependency
            // on guessing right.
            let input = tool_input?;
            let command = input
                .get("command")
                .or_else(|| input.get("cmd"))
                .or_else(|| input.get("script"))
                .and_then(Value::as_str)?;
            destructive_data_file_reason(command)
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
            match resolve_mutating_target_repo(tool_input.as_ref()) {
                // Target isn't in any git repo (or has no path at all and
                // cwd isn't one either) -- no guard.
                TargetRepo::Outside => (None, None),
                TargetRepo::Found(repo) => {
                    (current_branch(Some(&repo)), default_branch(Some(&repo)))
                }
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
    fn classify_blocks_rm_of_agentflare_db_via_bash() {
        let input = json!({ "command": "rm ~/.agentflare/store.db" });
        let reason = classify("Bash", Some(&input), NOT_A_REPO).unwrap();
        assert!(reason.contains("blocked"), "{reason}");
    }

    #[test]
    fn classify_blocks_del_of_agentflare_db_via_opencode_bash() {
        let input = json!({ "command": "del C:\\Users\\shiva\\.agentflare\\backend.db" });
        assert!(classify("bash", Some(&input), NOT_A_REPO).is_some());
    }

    #[test]
    fn classify_blocks_remove_item_via_powershell() {
        let input =
            json!({ "command": "Remove-Item $env:USERPROFILE\\.agentflare\\agentflare.db" });
        assert!(classify("PowerShell", Some(&input), NOT_A_REPO).is_some());
    }

    #[test]
    fn classify_blocks_recursive_delete_of_whole_agentflare_dir() {
        let input = json!({ "command": "rm -rf ~/.agentflare" });
        assert!(classify("Bash", Some(&input), NOT_A_REPO).is_some());
    }

    #[test]
    fn classify_blocks_rm_of_agentflare_db_via_ctx_shell() {
        // item #559: ctx_shell (direct tool name, matching what
        // unwrap_gateway_call produces for a gateway-routed call) must be
        // covered by the same destructive-command guard as Bash.
        let input = json!({ "command": "rm ~/.agentflare/store.db" });
        assert!(classify("mcp__lean-ctx__ctx_shell", Some(&input), NOT_A_REPO).is_some());
    }

    #[test]
    fn classify_allows_unrelated_rm_commands() {
        let input = json!({ "command": "rm /tmp/scratch.txt" });
        assert!(classify("Bash", Some(&input), NOT_A_REPO).is_none());
    }

    #[test]
    fn classify_allows_non_destructive_agentflare_db_commands() {
        let input = json!({ "command": "sqlite3 ~/.agentflare/store.db '.tables'" });
        assert!(classify("Bash", Some(&input), NOT_A_REPO).is_none());
    }

    #[test]
    fn classify_allows_commit_message_prose_mentioning_rm_and_db_paths() {
        // Regression: this exact scenario blocked the real commit that landed
        // this guard -- a `git commit` heredoc whose message describes the
        // incident in prose ("opencode ran an rm ... store.db ...
        // ~/.agentflare/*.db*") is not an executed rm, and must not match.
        let input = json!({ "command": "git commit -m \"$(cat <<'EOF'\nhook_redirect: block agent shell commands from deleting agentflare db files\n\nopencode ran an rm mid-migration and silently wiped store.db's metadata,\ntargeting ~/.agentflare/*.db* or the .agentflare dir itself.\nEOF\n)\"" });
        assert!(classify("Bash", Some(&input), NOT_A_REPO).is_none());
    }

    #[test]
    fn classify_blocks_rm_as_second_statement_after_separator() {
        let input = json!({ "command": "cd /tmp && rm ~/.agentflare/store.db" });
        assert!(classify("Bash", Some(&input), NOT_A_REPO).is_some());
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
    fn classify_reads_camel_case_file_path_for_spec_redirect() {
        let input = json!({ "filePath": "docs/superpowers/specs/foo.md" });
        assert!(classify("Write", Some(&input), ON_FEATURE_BRANCH).is_some());
    }

    #[test]
    fn redirect_decision_resolves_repo_from_camel_case_file_path() {
        // Regression: opencode's native edit/write send `filePath`; if the
        // key isn't parsed the target repo resolves to None and a default-
        // branch edit slips through.
        let tmp = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-b", "master"]);
        git(&[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "x",
        ]);
        std::fs::write(tmp.path().join("f.rs"), "x").unwrap();
        let input = json!({ "filePath": tmp.path().join("f.rs").to_string_lossy() });
        let decision = redirect_decision("edit", Some(&input))
            .expect("camelCase filePath must reach the branch guard");
        assert_eq!(decision["hookSpecificOutput"]["permissionDecision"], "deny");
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
    fn unwrap_gateway_call_unwraps_leanctx_execute() {
        let input = json!({
            "action": "execute",
            "server": "leanctx",
            "tool": "ctx_shell",
            "args": {"command": "cargo test"},
        });
        let (name, unwrapped) = unwrap_gateway_call(GATEWAY_TOOL_NAME, Some(&input));
        assert_eq!(name, "mcp__lean-ctx__ctx_shell");
        assert_eq!(unwrapped, Some(json!({"command": "cargo test"})));
    }

    #[test]
    fn unwrap_gateway_call_unwraps_ctx_patch_for_mutating_tool_classification() {
        let input = json!({
            "action": "execute",
            "server": "leanctx",
            "tool": "ctx_patch",
            "args": {"path": "src/main.rs"},
        });
        let (name, _) = unwrap_gateway_call(GATEWAY_TOOL_NAME, Some(&input));
        assert!(MUTATING_TOOLS.contains(&name.as_str()), "{name}");
    }

    #[test]
    fn unwrap_gateway_call_passes_through_non_gateway_tool() {
        let input = json!({"command": "cargo test"});
        let (name, unwrapped) = unwrap_gateway_call("Bash", Some(&input));
        assert_eq!(name, "Bash");
        assert_eq!(unwrapped, Some(input));
    }

    #[test]
    fn unwrap_gateway_call_passes_through_gateway_search_action() {
        let input = json!({"action": "search", "query": "ctx_shell"});
        let (name, unwrapped) = unwrap_gateway_call(GATEWAY_TOOL_NAME, Some(&input));
        assert_eq!(name, GATEWAY_TOOL_NAME);
        assert_eq!(unwrapped, Some(input));
    }

    #[test]
    fn unwrap_gateway_call_passes_through_non_leanctx_server() {
        let input = json!({"action": "execute", "server": "other", "tool": "foo", "args": {}});
        let (name, _) = unwrap_gateway_call(GATEWAY_TOOL_NAME, Some(&input));
        assert_eq!(name, GATEWAY_TOOL_NAME);
    }

    #[test]
    fn unwrap_gateway_call_handles_missing_args() {
        let input = json!({"action": "execute", "server": "leanctx", "tool": "ctx_shell"});
        let (name, unwrapped) = unwrap_gateway_call(GATEWAY_TOOL_NAME, Some(&input));
        assert_eq!(name, "mcp__lean-ctx__ctx_shell");
        assert_eq!(unwrapped, Some(json!({})));
    }

    #[test]
    fn completion_gate_blocks_item_done_without_verification() {
        let input = json!({ "id": "abc", "action": "done" });
        let reason = completion_gate_reason(ITEM_TOOL_NAME, Some(&input), false).unwrap();
        assert!(reason.contains("verification"), "{reason}");
        assert!(reason.contains("action=done"), "{reason}");
    }

    #[test]
    fn completion_gate_blocks_check_merge_without_verification() {
        let input = json!({ "id": "abc", "action": "check_merge" });
        assert!(completion_gate_reason(ITEM_TOOL_NAME, Some(&input), false).is_some());
    }

    #[test]
    fn completion_gate_allows_item_done_with_fresh_verification() {
        let input = json!({ "id": "abc", "action": "done" });
        assert!(completion_gate_reason(ITEM_TOOL_NAME, Some(&input), true).is_none());
    }

    #[test]
    fn completion_gate_ignores_other_item_actions() {
        let input = json!({ "id": "abc", "action": "claim" });
        assert!(completion_gate_reason(ITEM_TOOL_NAME, Some(&input), false).is_none());
        let input = json!({ "id": "abc", "action": "update" });
        assert!(completion_gate_reason(ITEM_TOOL_NAME, Some(&input), false).is_none());
    }

    #[test]
    fn completion_gate_ignores_unrelated_tools() {
        let input = json!({ "action": "done" });
        assert!(completion_gate_reason("Bash", Some(&input), false).is_none());
        assert!(completion_gate_reason("mcp__flare__comment", Some(&input), false).is_none());
    }

    #[test]
    fn completion_gate_ignores_missing_input_or_action() {
        assert!(completion_gate_reason(ITEM_TOOL_NAME, None, false).is_none());
        assert!(completion_gate_reason(ITEM_TOOL_NAME, Some(&json!({})), false).is_none());
    }

    #[test]
    fn completion_gate_matches_bare_item_tool_name() {
        let input = json!({ "id": "abc", "action": "done" });
        assert!(completion_gate_reason("item", Some(&input), false).is_some());
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
