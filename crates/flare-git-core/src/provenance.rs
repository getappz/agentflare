//! Commit provenance trailers — self-reported agent/branch/item/session
//! identity appended to commit messages via a `prepare-commit-msg` hook.
//! `agentflare git explain`/`rewind` (see `src/cli/git.rs`) read these back
//! off a commit to link it to the agent session (and, transitively, the
//! prompt) that produced it.
//!
//! Deliberately NOT cryptographically attested: agentflare has no
//! signing/binding system for this, so a trailer is a bare string an agent
//! could misreport — the same trust level as every other
//! `AGENTFLARE_AGENT`-based identity check already in this codebase (see
//! `claims::owner_id`'s identical fallback chain).

use std::path::Path;

use crate::branch::current_branch;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Trailers {
    pub agent: Option<String>,
    pub branch: Option<String>,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
}

/// Extracts the `<sequence_id>` from a `task/<sequence_id>` or
/// `task/<sequence_id>-<slug>` branch name (the `flare_git_core::worktree`
/// convention, see `task_branch_name`) -- only the leading digit run after
/// `task/` counts, so a slug suffix never leaks into the item id.
fn item_id_from_branch(branch: Option<&str>) -> Option<String> {
    let rest = branch?.strip_prefix("task/")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

/// Resolves the current session id: an explicit `AGENTFLARE_SESSION_ID`
/// override, falling back to `CLAUDE_CODE_SESSION_ID` -- set by Claude Code
/// on every session it runs, and the same id its local transcript
/// (`~/.claude/projects/<slug>/<session_id>.jsonl`) is keyed by. That
/// transcript is what `agentflare git explain` reads the originating
/// prompt back out of.
///
/// `CLAUDE_CODE_SESSION_ID` does the real work today: it's inherited by any
/// `git commit` a live Claude Code session runs directly (interactively, or
/// via its own Bash tool), which is how `explain`/`rewind` link a commit to
/// a session in practice. `AGENTFLARE_SESSION_ID` is an override escape
/// hatch in the same spirit as this crate's `AGENTFLARE_AGENT`/
/// `AGENTFLARE_SESSION` -- for a caller that already knows the session id
/// it wants stamped and isn't relying on env inheritance. No in-repo
/// dispatch path sets it (unlike `AGENTFLARE_AGENT`, which `agent_launch.rs`
/// and `work.rs` genuinely set before spawning): the one headless
/// coding-agent spawn path (`agent_launch.rs::run_headless`) doesn't know
/// the child's session id until the child reports it in its own post-hoc
/// JSON reply, well after any commit it makes has already happened. A
/// headless dispatch commit therefore gets a session trailer only when
/// `CLAUDE_CODE_SESSION_ID` happens to be inherited (i.e. the child is
/// itself Claude Code and passes its env through) -- otherwise it has none,
/// which `explain`/`rewind` report accurately as "commit ... wasn't
/// agent-made" rather than falsely attributing it.
fn resolve_session_id() -> Option<String> {
    std::env::var("AGENTFLARE_SESSION_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("CLAUDE_CODE_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty())
        })
}

/// Resolves the current commit's provenance: agent identity
/// (`AGENTFLARE_AGENT`, falling back to auto-detection), the current
/// branch, the item id it belongs to (if the branch matches the
/// `task/<sequence_id>` convention `flare_git_core::worktree` uses), and the
/// agent session id (see `resolve_session_id`).
#[must_use]
pub fn build_trailers(repo_root: &Path) -> Trailers {
    let agent = std::env::var("AGENTFLARE_AGENT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(agent_detector::agent_name);
    let branch = current_branch(repo_root);
    let item_id = item_id_from_branch(branch.as_deref());
    let session_id = resolve_session_id();
    Trailers {
        agent,
        branch,
        item_id,
        session_id,
    }
}

/// Appends non-empty `Trailers` fields to `msg` as git trailers, skipping
/// any field that didn't resolve rather than writing an empty trailer.
/// A no-op if `msg` already carries agentflare trailers (e.g. `commit
/// --amend` re-invokes `prepare-commit-msg` on an already-stamped
/// message) — never duplicates.
#[must_use]
pub fn append_trailers(msg: &str, t: &Trailers) -> String {
    if msg.contains("Agentflare-Agent:")
        || msg.contains("Agentflare-Branch:")
        || msg.contains("Agentflare-Item:")
        || msg.contains("Agentflare-Session:")
    {
        return msg.to_string();
    }
    let mut lines = Vec::new();
    if let Some(agent) = &t.agent {
        lines.push(format!("Agentflare-Agent: {agent}"));
    }
    if let Some(branch) = &t.branch {
        lines.push(format!("Agentflare-Branch: {branch}"));
    }
    if let Some(item_id) = &t.item_id {
        lines.push(format!("Agentflare-Item: {item_id}"));
    }
    if let Some(session_id) = &t.session_id {
        lines.push(format!("Agentflare-Session: {session_id}"));
    }
    if lines.is_empty() {
        return msg.to_string();
    }
    let mut out = msg.trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(&lines.join("\n"));
    out.push('\n');
    out
}

/// The inverse of `append_trailers` -- reads a commit message back into a
/// `Trailers`, tolerating any surrounding message body. Unknown fields are
/// simply absent (`None`), never an error: a commit made before this field
/// existed, or by a non-agentflare tool, is just a `Trailers::default()`.
/// Used by `agentflare git explain`/`rewind` to recover the provenance of an
/// already-made commit.
#[must_use]
pub fn parse_trailers(msg: &str) -> Trailers {
    let mut t = Trailers::default();
    for line in msg.lines() {
        if let Some(v) = line.strip_prefix("Agentflare-Agent: ") {
            t.agent = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Agentflare-Branch: ") {
            t.branch = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Agentflare-Item: ") {
            t.item_id = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Agentflare-Session: ") {
            t.session_id = Some(v.to_string());
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trailers() -> Trailers {
        Trailers {
            agent: Some("claude-code".to_string()),
            branch: Some("task/42".to_string()),
            item_id: Some("42".to_string()),
            session_id: Some("sess-abc".to_string()),
        }
    }

    #[test]
    fn appends_all_resolved_fields_after_a_blank_line() {
        let out = append_trailers("fix: thing\n", &trailers());
        assert_eq!(
            out,
            "fix: thing\n\nAgentflare-Agent: claude-code\nAgentflare-Branch: task/42\nAgentflare-Item: 42\nAgentflare-Session: sess-abc\n"
        );
    }

    #[test]
    fn skips_unresolved_fields_entirely() {
        let t = Trailers {
            agent: Some("claude-code".to_string()),
            branch: None,
            item_id: None,
            session_id: None,
        };
        let out = append_trailers("fix: thing\n", &t);
        assert_eq!(out, "fix: thing\n\nAgentflare-Agent: claude-code\n");
    }

    #[test]
    fn parse_trailers_round_trips_append_trailers() {
        let stamped = append_trailers("fix: thing\n", &trailers());
        assert_eq!(parse_trailers(&stamped), trailers());
    }

    #[test]
    fn parse_trailers_on_a_plain_message_is_all_none() {
        assert_eq!(parse_trailers("fix: thing\n"), Trailers::default());
    }

    #[test]
    fn returns_message_unchanged_when_nothing_resolved() {
        let out = append_trailers("fix: thing\n", &Trailers::default());
        assert_eq!(out, "fix: thing\n");
    }

    #[test]
    fn does_not_duplicate_trailers_on_an_already_stamped_message() {
        let once = append_trailers("fix: thing\n", &trailers());
        let twice = append_trailers(&once, &trailers());
        assert_eq!(once, twice);
    }

    #[test]
    fn item_id_extracted_from_task_branch_convention() {
        // Mirrors flare_git_core::worktree's bare `task/<sequence_id>` form.
        assert_eq!(item_id_from_branch(Some("task/17")).as_deref(), Some("17"));
    }

    #[test]
    fn item_id_extracted_from_slugged_task_branch_convention() {
        // Mirrors flare_git_core::worktree::task_branch_name's slugged
        // `task/<sequence_id>-<slug>` form -- only the leading digit run
        // counts, not the whole remainder after `task/`.
        assert_eq!(
            item_id_from_branch(Some("task/33-agentflare-code-review")).as_deref(),
            Some("33")
        );
    }

    #[test]
    fn item_id_none_for_a_non_task_branch() {
        assert_eq!(item_id_from_branch(Some("main")), None);
        assert_eq!(item_id_from_branch(Some("task/")), None);
        assert_eq!(item_id_from_branch(None), None);
    }
}
