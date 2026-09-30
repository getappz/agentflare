// PreToolUse guard: refuse writes into a claimed item's worktree
// (`.worktrees/task/<N>/`) unless the caller owns the live claim (item #609).
// A soft "held" status is what let a second session edit a worktree a
// dispatched job was working in; this is the hard deny behind it.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Live claim on an item: `(owner, age_secs)`.
type LiveClaim = (String, i64);

/// Item sequence id if `path` sits under `.worktrees/task/<N>/...`. Splits on
/// both separators (a Windows path must parse on any host) and resolves `..`
/// lexically, so `.worktrees/task/1/../../..` is not still "in" item 1.
fn item_seq_for_path(path: &Path) -> Option<String> {
    let mut stack: Vec<&str> = Vec::new();
    for c in path.to_str()?.split(['/', '\\']) {
        match c {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            c => stack.push(c),
        }
    }
    stack.windows(3).find_map(|w| match w {
        [".worktrees", "task", seq] if seq.bytes().all(|b| b.is_ascii_digit()) => {
            Some(seq.to_string())
        }
        _ => None,
    })
}

const GIT_WRITE_SUBCOMMANDS: &[&str] = &[
    "commit",
    "add",
    "push",
    "pull",
    "merge",
    "rebase",
    "reset",
    "checkout",
    "switch",
    "restore",
    "stash",
    "clean",
    "cherry-pick",
    "revert",
    "rm",
    "mv",
    "apply",
    "am",
];

/// Directories a shell command runs a mutating git subcommand in. Splits on
/// `&&`/`||`/`;`/`|`/newlines so `echo git commit` (prose) never matches, and
/// resolves `cd <dir> && git ...` and `git -C <dir> ...` against `cwd` -- a
/// command run from outside the worktree still counts. Quoting/subshells are
/// not parsed: a best-effort guard, not a sandbox (see item #482).
fn git_write_dirs(command: &str, cwd: &Path) -> Vec<PathBuf> {
    let mut base = cwd.to_path_buf();
    let mut dirs = Vec::new();
    let normalized = command
        .replace("&&", "\n")
        .replace("||", "\n")
        .replace([';', '|'], "\n");
    for segment in normalized.lines() {
        let mut toks = segment
            .split_whitespace()
            .map(|t| t.trim_matches(['"', '\'']));
        match toks.next() {
            Some("cd") => {
                if let Some(dir) = toks.next() {
                    base = base.join(dir);
                }
            }
            Some("git") => {
                let mut dir = base.clone();
                let mut sub = None;
                while let Some(t) = toks.next() {
                    if let Some(d) = t.strip_prefix("--work-tree=") {
                        dir = dir.join(d);
                        continue;
                    }
                    match t {
                        "-C" | "--work-tree" => {
                            if let Some(d) = toks.next() {
                                dir = dir.join(d);
                            }
                        }
                        // Global options that take their operand as the next
                        // token; without this the operand reads as the subcommand.
                        "-c" | "--git-dir" | "--namespace" => {
                            toks.next();
                        }
                        _ if t.starts_with('-') => {}
                        _ => {
                            sub = Some(t);
                            break;
                        }
                    }
                }
                if sub.is_some_and(|s| GIT_WRITE_SUBCOMMANDS.contains(&s)) {
                    dirs.push(dir);
                }
            }
            _ => {}
        }
    }
    dirs
}

/// Paths whose worktree membership decides the guard: the target file of a
/// mutating tool, or the directories a shell call runs mutating git in.
fn write_targets(tool_name: &str, tool_input: Option<&Value>) -> Vec<PathBuf> {
    if crate::hook_redirect::MUTATING_TOOLS.contains(&tool_name) {
        let path = tool_input.and_then(|ti| {
            ti.get("file_path")
                .or_else(|| ti.get("path"))
                .or_else(|| ti.get("filePath"))
                .and_then(Value::as_str)
        });
        // A relative path is resolved against the cwd (an absolute one
        // replaces it in `join`); no cwd -> no target, so the guard stays
        // silent instead of judging a path it cannot place.
        let cwd = std::env::current_dir().ok();
        return match (path, cwd) {
            (Some(p), Some(cwd)) => vec![cwd.join(p)],
            // Same fallback as the branch guard: no path -> the cwd.
            (None, Some(cwd)) => vec![cwd],
            (_, None) => Vec::new(),
        };
    }
    if matches!(
        tool_name,
        "Bash" | "bash" | "PowerShell" | "powershell" | "shell" | "mcp__lean-ctx__ctx_shell"
    ) && let Some(input) = tool_input
        && let Some(command) = input
            .get("command")
            .or_else(|| input.get("cmd"))
            .or_else(|| input.get("script"))
            .and_then(Value::as_str)
        && let Ok(cwd) = std::env::current_dir()
    {
        return git_write_dirs(command, &cwd);
    }
    Vec::new()
}

/// `<agent>:<pid>-<16 hex>`: the per-process owner id `claims::owner_id()`
/// falls back to when no `AGENTFLARE_CLAIM_OWNER`/`AGENTFLARE_SESSION` pins
/// one. Dispatched jobs always pin theirs, so they never look like this.
fn is_process_local_owner(owner: &str) -> bool {
    owner.split_once(':').is_some_and(|(_, inst)| {
        inst.split_once('-').is_some_and(|(pid, rand)| {
            !pid.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
                && rand.len() == 16
                && rand.bytes().all(|b| b.is_ascii_hexdigit())
        })
    })
}

/// Whether `caller` (the hook process's `owner_id()`) is the claim's owner.
/// An interactive session's MCP server (which made the claim) and its hook
/// process are different processes with different per-process ids, so two
/// process-local ids of the same agent count as the same session; the
/// residual gap (two interactive sessions of one agent) is still stopped at
/// claim time by the `item claim` error. Pinned owners (dispatched jobs) must
/// match exactly.
fn caller_is_owner(caller: &str, owner: &str) -> bool {
    caller == owner
        || (is_process_local_owner(caller)
            && is_process_local_owner(owner)
            && crate::claims::agent_of(caller) == crate::claims::agent_of(owner))
}

/// Pure core: deny reason when `path` is in item N's worktree and `lookup(N)`
/// reports live claims none of which `caller` owns. `lookup` returns every
/// live claim across items sharing sequence number N (one per project), so a
/// collision fails closed instead of open.
fn deny_reason(
    path: &Path,
    caller: &str,
    lookup: impl FnOnce(&str) -> Vec<LiveClaim>,
) -> Option<String> {
    let seq = item_seq_for_path(path)?;
    let claims = lookup(&seq);
    if claims
        .iter()
        .any(|(owner, _)| caller_is_owner(caller, owner))
    {
        return None;
    }
    let (owner, age_secs) = claims.into_iter().next()?;
    Some(format!(
        "item {seq} is claimed by {owner} (active {age_secs}s ago) -- do not edit its worktree \
         ({caller} is not the owner). Wait for it to finish, or take over explicitly: release \
         the claim (item action=release) or steal it after the TTL expires, then claim it yourself."
    ))
}

/// Pure core for the mirror case (item #641): writes in the shared main
/// checkout while it sits on a non-default branch. The default branch is
/// already denied by the branch guard, so only feature branches reach here.
/// Denies when other sessions hold live claims in this repo and `caller` owns
/// none. `bypass` (`AGENTFLARE_GIT_BYPASS`, the git shim's escape hatch) allows.
fn main_checkout_deny_reason(
    bypass: bool,
    is_linked_worktree: bool,
    branch: Option<&str>,
    default: Option<&str>,
    caller: &str,
    repo_claims: impl FnOnce() -> Vec<(String, String, i64)>,
) -> Option<String> {
    let branch = branch?;
    if bypass
        || is_linked_worktree
        || crate::hook_redirect::branch_guard_reason_for(Some(branch), default).is_some()
    {
        return None;
    }
    let claims = repo_claims();
    if claims.is_empty() || claims.iter().any(|(_, o, _)| caller_is_owner(caller, o)) {
        return None;
    }
    let (seq, owner, age_secs) = claims.into_iter().next()?;
    Some(format!(
        "the shared main checkout (on '{branch}') is not yours to edit: item {seq} is claimed by \
         {owner} (active {age_secs}s ago) and {caller} holds no claim, so uncommitted changes here \
         may belong to another session. Claim an item (item action=claim) and work in its \
         worktree instead."
    ))
}

/// Main-checkout guard for one write target, resolving git state for it.
fn main_checkout_deny(path: &Path, caller: &str) -> Option<String> {
    if item_seq_for_path(path).is_some() {
        return None;
    }
    let start = path.ancestors().find(|a| a.is_dir())?;
    let repo = flare_git_core::branch::repo_toplevel(start)?;
    main_checkout_deny_reason(
        std::env::var_os("AGENTFLARE_GIT_BYPASS").is_some(),
        flare_git_core::branch::is_linked_worktree(&repo),
        flare_git_core::branch::current_branch(&repo).as_deref(),
        Some(flare_git_core::branch::resolve_default_branch(&repo)).as_deref(),
        caller,
        || repo_live_claims_from_db(&repo),
    )
}

/// `(item seq, owner, age_secs)` for live claims on items whose worktree
/// `<repo>/.worktrees/task/<seq>` exists in THIS repo. The hook process cannot
/// resolve the project, and `backend.db` is shared across projects that number
/// their items independently, so that worktree directory is what ties a claim
/// to this repo. Empty on any error, so a broken DB or directory never blocks
/// edits.
fn repo_live_claims_in(conn: &rusqlite::Connection, repo: &Path) -> Vec<(String, String, i64)> {
    let Ok(dir) = std::fs::read_dir(repo.join(".worktrees").join("task")) else {
        return Vec::new();
    };
    dir.filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .flat_map(|seq| {
            live_claims_in(conn, &seq)
                .into_iter()
                .map(move |(owner, age)| (seq.clone(), owner, age))
        })
        .collect()
}

fn repo_live_claims_from_db(repo: &Path) -> Vec<(String, String, i64)> {
    let db_path = crate::paths::agentflare_dir().join("backend.db");
    if !db_path.exists() {
        return Vec::new();
    }
    agentflare_backend::db::open_db(&db_path)
        .map(|conn| repo_live_claims_in(&conn, repo))
        .unwrap_or_default()
}

/// Live claims on every non-deleted item with sequence number `seq`. Empty
/// on a lookup error, so a broken DB never blocks edits.
fn live_claims_in(conn: &rusqlite::Connection, seq: &str) -> Vec<LiveClaim> {
    let ids: Vec<String> = (|| {
        let mut stmt = conn
            .prepare("SELECT id FROM items WHERE sequence_id = ?1 AND deleted_at IS NULL")
            .ok()?;
        let rows = stmt.query_map([seq], |r| r.get::<_, String>(0)).ok()?;
        Some(rows.filter_map(Result::ok).collect())
    })()
    .unwrap_or_default();
    let ttl = crate::mcp_server::types::backend_claim_ttl_secs();
    let now = db_kit::ids::now();
    ids.iter()
        .filter_map(|id| {
            agentflare_backend::claim::live_claim_on_item(conn, id, now, ttl)
                .ok()
                .flatten()
        })
        .map(|c| (c.owner, c.age_secs))
        .collect()
}

fn live_claims_from_db(seq: &str) -> Vec<LiveClaim> {
    let db_path = crate::paths::agentflare_dir().join("backend.db");
    if !db_path.exists() {
        return Vec::new();
    }
    match agentflare_backend::db::open_db(&db_path) {
        Ok(conn) => live_claims_in(&conn, seq),
        Err(_) => Vec::new(),
    }
}

/// PreToolUse deny decision, or `None` to let the call through. Runs under the
/// same hard timeout as the branch guard: the main-checkout rule shells out to
/// git, and a slow repo must fail open, not stall every tool call.
pub fn claim_guard_decision(tool_name: &str, tool_input: Option<&Value>) -> Option<Value> {
    let tool_name = tool_name.to_string();
    let tool_input = tool_input.cloned();
    crate::hook_redirect::decide_with_timeout(crate::hook_redirect::GATING_TIMEOUT, move || {
        let caller = crate::claims::owner_id();
        let reason = write_targets(&tool_name, tool_input.as_ref())
            .iter()
            .find_map(|path| {
                deny_reason(path, &caller, live_claims_from_db)
                    .or_else(|| main_checkout_deny(path, &caller))
            })?;
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

    const WT: &str = "/r/.worktrees/task/607/src/cancel.rs";
    const JOB: &str = "claude-code:c2Y8uM0G6XTjLtEyOinGl";
    const INTERACTIVE_A: &str = "claude-code:4242-0123456789abcdef";
    const INTERACTIVE_B: &str = "claude-code:9999-fedcba9876543210";

    fn claims(owner: &str) -> impl FnOnce(&str) -> Vec<LiveClaim> {
        let owner = owner.to_string();
        move |_| vec![(owner, 42)]
    }

    #[test]
    fn seq_parsed_from_worktree_paths_both_separators() {
        for p in [
            "/r/.worktrees/task/609/src/a.rs",
            r"C:\r\.worktrees\task\609\src\a.rs",
            "/r/.worktrees/task/609",
        ] {
            assert_eq!(
                item_seq_for_path(Path::new(p)).as_deref(),
                Some("609"),
                "{p}"
            );
        }
    }

    #[test]
    fn non_worktree_paths_have_no_item() {
        for p in [
            "/r/src/a.rs",
            "/r/.worktrees/other/609/a",
            "/r/.worktrees/task/abc/a",
            "/r/.worktrees/task/1/../../../src/a.rs",
            r"C:\r\.worktrees\task\1\..\..\..\src\a.rs",
        ] {
            assert_eq!(item_seq_for_path(Path::new(p)), None, "{p}");
        }
    }

    #[test]
    fn interactive_session_is_denied_on_a_job_claim_with_actionable_message() {
        let r = deny_reason(Path::new(WT), INTERACTIVE_A, claims(JOB)).expect("deny");
        assert!(r.contains(&format!("claimed by {JOB}")), "{r}");
        assert!(r.contains("release"), "{r}");
    }

    #[test]
    fn pinned_owner_is_allowed_and_other_pinned_owner_denied() {
        assert_eq!(deny_reason(Path::new(WT), JOB, claims(JOB)), None);
        assert!(deny_reason(Path::new(WT), "claude-code:otherJob", claims(JOB)).is_some());
    }

    #[test]
    fn interactive_owner_is_recognized_across_mcp_and_hook_processes() {
        // The MCP server (claimant) and the hook are different processes, so
        // their per-process ids differ; same agent still counts as the owner.
        assert_eq!(
            deny_reason(Path::new(WT), INTERACTIVE_B, claims(INTERACTIVE_A)),
            None
        );
        // ...but a different agent does not.
        assert!(
            deny_reason(
                Path::new(WT),
                "codex:9999-fedcba9876543210",
                claims(INTERACTIVE_A)
            )
            .is_some()
        );
    }

    #[test]
    fn colliding_sequence_numbers_fail_closed_unless_caller_owns_one() {
        let two = |_: &str| vec![(JOB.to_string(), 5), ("other:job".to_string(), 9)];
        assert!(deny_reason(Path::new(WT), INTERACTIVE_A, two).is_some());
        assert_eq!(deny_reason(Path::new(WT), "other:job", two), None);
    }

    #[test]
    fn main_checkout_on_feature_branch_is_denied_when_others_hold_claims() {
        let others = || vec![("607".to_string(), JOB.to_string(), 9)];
        fn deny(
            bypass: bool,
            linked: bool,
            branch: Option<&str>,
            default: Option<&str>,
            caller: &str,
            claims: impl FnOnce() -> Vec<(String, String, i64)>,
        ) -> Option<String> {
            main_checkout_deny_reason(bypass, linked, branch, default, caller, claims)
        }
        let r = deny(
            false,
            false,
            Some("feat/x"),
            Some("master"),
            INTERACTIVE_A,
            others,
        )
        .expect("deny");
        assert!(r.contains("feat/x") && r.contains(JOB), "{r}");
        // bypass, caller owns a claim, no claims at all, linked worktree, default
        // branch (branch guard's job), or no branch -> allowed.
        let mine = || vec![("1".to_string(), JOB.to_string(), 9)];
        let b = Some("feat/x");
        let d = Some("master");
        assert_eq!(deny(true, false, b, d, INTERACTIVE_A, others), None);
        assert_eq!(deny(false, false, b, d, JOB, mine), None);
        assert_eq!(deny(false, false, b, d, "me", Vec::new), None);
        assert_eq!(deny(false, true, b, d, "me", others), None);
        assert_eq!(deny(false, false, d, d, "me", others), None);
        assert_eq!(deny(false, false, None, d, "me", others), None);
    }

    /// Real `backend.db`: only claims whose `.worktrees/task/<seq>` exists in
    /// THIS repo count, so a live claim in another project that happens to
    /// share nothing with this checkout never blocks it.
    #[test]
    fn repo_live_claims_only_count_items_with_a_worktree_in_this_repo() {
        use crate::mcp_server::AgentflareMcp;
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp::for_test(
            tmp.path().join("backend.db"),
            tmp.path().to_path_buf(),
            tmp.path().join("project.json"),
        );
        s.with_backend_db(|conn| {
            let project = s.resolve_project(conn).unwrap();
            let state = agentflare_backend::state::list_by_project(conn, &project.id)
                .unwrap()
                .into_iter()
                .find(|st| st.is_default)
                .unwrap();
            let ttl = crate::mcp_server::types::backend_claim_ttl_secs();
            let mut seqs = Vec::new();
            for name in ["Here", "Elsewhere"] {
                let item = agentflare_backend::item::create(
                    conn,
                    agentflare_backend::item::CreateItem {
                        project_id: project.id.clone(),
                        state_id: state.id.clone(),
                        name: name.into(),
                        description: None,
                        priority: None,
                        parent_id: None,
                        assignee_agent: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                        metadata: None,
                        label_ids: vec![],
                        assignee_ids: vec![],
                        dependency_ids: vec![],
                        start_date: None,
                        due_date: None,
                    },
                )
                .unwrap();
                agentflare_backend::item::claim(conn, &item.id, JOB, crate::claims::now(), ttl)
                    .unwrap();
                seqs.push(item.sequence_id);
            }
            let repo = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(repo.path().join(format!(".worktrees/task/{}", seqs[0])))
                .unwrap();
            let got = repo_live_claims_in(conn, repo.path());
            assert_eq!(
                got,
                vec![(seqs[0].to_string(), JOB.to_string(), got[0].2)],
                "only the item with a worktree here counts"
            );
            // No `.worktrees/task` at all -> nothing counts.
            let empty = tempfile::tempdir().unwrap();
            assert!(repo_live_claims_in(conn, empty.path()).is_empty());
        })
        .unwrap();
    }

    #[test]
    fn unclaimed_item_and_outside_paths_are_allowed() {
        assert_eq!(deny_reason(Path::new(WT), "me", |_| Vec::new()), None);
        assert_eq!(
            deny_reason(Path::new("/r/src/a.rs"), "me", |_| panic!("no lookup")),
            None
        );
    }

    #[test]
    fn git_write_dirs_handles_dash_c_cd_and_ignores_prose() {
        let cwd = Path::new("/r");
        assert_eq!(
            git_write_dirs("git commit -m x", cwd),
            vec![PathBuf::from("/r")]
        );
        assert_eq!(
            git_write_dirs("git -C .worktrees/task/1 commit -m x", cwd),
            vec![PathBuf::from("/r/.worktrees/task/1")]
        );
        assert_eq!(
            git_write_dirs("cd .worktrees/task/1 && git add -A", cwd),
            vec![PathBuf::from("/r/.worktrees/task/1")]
        );
        assert!(git_write_dirs("echo git commit", cwd).is_empty());
        assert!(git_write_dirs("git status && git log", cwd).is_empty());
    }

    #[test]
    fn git_write_dirs_skips_global_option_operands_and_sees_destructive_subcommands() {
        let cwd = Path::new("/r");
        // `-c k=v` must not read `k=v` as the subcommand.
        assert_eq!(
            git_write_dirs("git -c user.name=x commit -m y", cwd),
            vec![PathBuf::from("/r")]
        );
        // The effective work tree, in both spellings, is the write target.
        let wt = vec![PathBuf::from("/r/.worktrees/task/2")];
        assert_eq!(
            git_write_dirs("git --work-tree .worktrees/task/2 checkout .", cwd),
            wt
        );
        assert_eq!(
            git_write_dirs("git --work-tree=.worktrees/task/2 add -A", cwd),
            wt
        );
        for sub in [
            "checkout",
            "switch",
            "restore",
            "stash",
            "clean",
            "cherry-pick",
        ] {
            assert_eq!(
                git_write_dirs(&format!("git {sub} x"), cwd).len(),
                1,
                "{sub}"
            );
        }
        assert!(git_write_dirs("git --git-dir .git status", cwd).is_empty());
    }

    #[test]
    fn relative_tool_paths_are_resolved_against_the_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let rel = json!({"file_path": ".worktrees/task/1/a.rs"});
        assert_eq!(
            write_targets("Edit", Some(&rel)),
            vec![cwd.join(".worktrees/task/1/a.rs")]
        );
    }

    #[test]
    fn write_targets_cover_mutating_tools_only() {
        let edit = json!({"file_path": "/r/.worktrees/task/1/a.rs"});
        assert_eq!(write_targets("Edit", Some(&edit)).len(), 1);
        assert!(write_targets("Read", Some(&edit)).is_empty());
        let status = json!({"command": "git status"});
        assert!(write_targets("Bash", Some(&status)).is_empty());
    }

    /// Real `backend.db`: an item claimed by a dispatched job (owner pinned,
    /// as `AGENTFLARE_CLAIM_OWNER` does) denies a different session and
    /// allows the owner.
    #[test]
    fn real_db_claim_denies_non_owner_and_allows_owner() {
        use crate::mcp_server::AgentflareMcp;
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp::for_test(
            tmp.path().join("backend.db"),
            tmp.path().to_path_buf(),
            tmp.path().join("project.json"),
        );
        s.with_backend_db(|conn| {
            let project = s.resolve_project(conn).unwrap();
            let state = agentflare_backend::state::list_by_project(conn, &project.id)
                .unwrap()
                .into_iter()
                .find(|st| st.is_default)
                .unwrap();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id,
                    state_id: state.id,
                    name: "Guarded".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                    start_date: None,
                    due_date: None,
                },
            )
            .unwrap();
            let (id, seq) = (item.id, item.sequence_id);
            let path = PathBuf::from(format!("/r/.worktrees/task/{seq}/src/a.rs"));
            let ttl = crate::mcp_server::types::backend_claim_ttl_secs();
            agentflare_backend::item::claim(conn, &id, JOB, crate::claims::now(), ttl).unwrap();
            let lookup = |seq: &str| live_claims_in(conn, seq);

            let denied = crate::claims::with_owner_override(INTERACTIVE_A, || {
                deny_reason(&path, &crate::claims::owner_id(), lookup)
            });
            assert!(denied.is_some_and(|r| r.contains(JOB)));
            let allowed = crate::claims::with_owner_override(JOB, || {
                deny_reason(&path, &crate::claims::owner_id(), lookup)
            });
            assert_eq!(allowed, None);
        })
        .unwrap();
    }
}
