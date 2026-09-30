// PreToolUse guard: refuse writes into a claimed item's worktree
// (`.worktrees/task/<N>/`) unless the caller owns the live claim (item #609).
// A soft "held" status is what let a second session edit a worktree a
// dispatched job was working in; this is the hard deny behind it.
use serde_json::{Value, json};
use std::path::{Component, Path};

/// Live claim on an item: `(owner, age_secs)`.
type LiveClaim = (String, i64);

/// Item sequence id if `path` sits under `.worktrees/task/<N>/...`.
fn item_seq_for_path(path: &Path) -> Option<String> {
    let mut comps = path.components().filter_map(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    while let Some(c) = comps.next() {
        if c == ".worktrees" && comps.next() == Some("task") {
            let seq = comps.next()?;
            return (!seq.is_empty() && seq.bytes().all(|b| b.is_ascii_digit()))
                .then(|| seq.to_string());
        }
    }
    None
}

fn is_git_write_command(command: &str) -> bool {
    [
        "git commit",
        "git add",
        "git push",
        "git merge",
        "git rebase",
        "git reset",
    ]
    .iter()
    .any(|g| command.contains(g))
}

/// The path whose worktree membership decides the guard, if this tool call
/// is a write: the target file of a mutating tool, or the cwd of a shell
/// call running a mutating git command.
fn write_target(tool_name: &str, tool_input: Option<&Value>) -> Option<std::path::PathBuf> {
    if crate::hook_redirect::MUTATING_TOOLS.contains(&tool_name) {
        let path = tool_input.and_then(|ti| {
            ti.get("file_path")
                .or_else(|| ti.get("path"))
                .or_else(|| ti.get("filePath"))
                .and_then(Value::as_str)
        });
        return match path {
            Some(p) => Some(p.into()),
            None => std::env::current_dir().ok(),
        };
    }
    if matches!(
        tool_name,
        "Bash" | "bash" | "PowerShell" | "powershell" | "shell" | "mcp__lean-ctx__ctx_shell"
    ) {
        let input = tool_input?;
        let command = input
            .get("command")
            .or_else(|| input.get("cmd"))
            .or_else(|| input.get("script"))
            .and_then(Value::as_str)?;
        if is_git_write_command(command) {
            return std::env::current_dir().ok();
        }
    }
    None
}

/// Pure core: deny reason when `path` is in item N's worktree and `lookup(N)`
/// reports a live claim owned by someone other than `caller`.
fn deny_reason(
    path: &Path,
    caller: &str,
    lookup: impl FnOnce(&str) -> Option<LiveClaim>,
) -> Option<String> {
    let seq = item_seq_for_path(path)?;
    let (owner, age_secs) = lookup(&seq)?;
    (owner != caller).then(|| {
        format!(
            "item {seq} is claimed by {owner} (active {age_secs}s ago) -- do not edit its worktree \
             ({caller} is not the owner). Wait for it to finish, or take over explicitly: release \
             the claim (item action=release) or steal it after the TTL expires, then claim it yourself."
        )
    })
}

/// Live claim on item `seq` from `backend.db`. Fails open (`None`) on any
/// lookup error so a broken DB never blocks edits.
fn live_claim_from_db(seq: &str) -> Option<LiveClaim> {
    let db_path = crate::paths::agentflare_dir().join("backend.db");
    if !db_path.exists() {
        return None;
    }
    let conn = agentflare_backend::db::open_db(&db_path).ok()?;
    let item_id = agentflare_backend::item::resolve_id(&conn, None, seq).ok()?;
    let ttl = crate::mcp_server::types::backend_claim_ttl_secs();
    agentflare_backend::claim::live_claim_on_item(&conn, &item_id, db_kit::ids::now(), ttl)
        .ok()
        .flatten()
        .map(|c| (c.owner, c.age_secs))
}

/// PreToolUse deny decision, or `None` to let the call through.
pub fn claim_guard_decision(tool_name: &str, tool_input: Option<&Value>) -> Option<Value> {
    let path = write_target(tool_name, tool_input)?;
    let reason = deny_reason(&path, &crate::claims::owner_id(), live_claim_from_db)?;
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(owner: &str) -> impl FnOnce(&str) -> Option<LiveClaim> {
        let owner = owner.to_string();
        move |_| Some((owner, 42))
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
        ] {
            assert_eq!(item_seq_for_path(Path::new(p)), None, "{p}");
        }
    }

    #[test]
    fn non_owner_is_denied_with_actionable_message() {
        let r = deny_reason(
            Path::new("/r/.worktrees/task/607/src/cancel.rs"),
            "claude-code:interactive",
            claim("claude-code:job"),
        )
        .expect("deny");
        assert!(r.contains("claimed by claude-code:job"), "{r}");
        assert!(r.contains("release"), "{r}");
    }

    #[test]
    fn owner_is_allowed() {
        let allowed = deny_reason(
            Path::new("/r/.worktrees/task/607/src/cancel.rs"),
            "claude-code:job",
            claim("claude-code:job"),
        );
        assert_eq!(allowed, None);
    }

    #[test]
    fn unclaimed_item_and_outside_paths_are_allowed() {
        let p = Path::new("/r/.worktrees/task/607/a.rs");
        assert_eq!(deny_reason(p, "me", |_| None), None);
        // Lookup must not even run for a path outside any task worktree.
        assert_eq!(
            deny_reason(Path::new("/r/src/a.rs"), "me", |_| panic!("no lookup")),
            None
        );
    }

    #[test]
    fn write_target_covers_mutating_tools_and_git_writes_only() {
        let edit = json!({"file_path": "/r/.worktrees/task/1/a.rs"});
        assert!(write_target("Edit", Some(&edit)).is_some());
        assert!(write_target("Read", Some(&edit)).is_none());
        let commit = json!({"command": "git commit -m x"});
        assert!(write_target("Bash", Some(&commit)).is_some());
        let status = json!({"command": "git status"});
        assert!(write_target("Bash", Some(&status)).is_none());
    }
}
