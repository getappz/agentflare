//! Commit provenance trailers — self-reported agent/branch/item identity
//! appended to commit messages via a `prepare-commit-msg` hook.
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

/// Resolves the current commit's provenance: agent identity
/// (`AGENTFLARE_AGENT`, falling back to auto-detection), the current
/// branch, and — if the branch matches the `task/<sequence_id>` convention
/// `flare_git_core::worktree` uses — the item id it belongs to.
#[must_use]
pub fn build_trailers(repo_root: &Path) -> Trailers {
    let agent = std::env::var("AGENTFLARE_AGENT")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(agent_detector::agent_name);
    let branch = current_branch(repo_root);
    let item_id = item_id_from_branch(branch.as_deref());
    Trailers {
        agent,
        branch,
        item_id,
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
    if lines.is_empty() {
        return msg.to_string();
    }
    let mut out = msg.trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(&lines.join("\n"));
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trailers() -> Trailers {
        Trailers {
            agent: Some("claude-code".to_string()),
            branch: Some("task/42".to_string()),
            item_id: Some("42".to_string()),
        }
    }

    #[test]
    fn appends_all_resolved_fields_after_a_blank_line() {
        let out = append_trailers("fix: thing\n", &trailers());
        assert_eq!(
            out,
            "fix: thing\n\nAgentflare-Agent: claude-code\nAgentflare-Branch: task/42\nAgentflare-Item: 42\n"
        );
    }

    #[test]
    fn skips_unresolved_fields_entirely() {
        let t = Trailers {
            agent: Some("claude-code".to_string()),
            branch: None,
            item_id: None,
        };
        let out = append_trailers("fix: thing\n", &t);
        assert_eq!(out, "fix: thing\n\nAgentflare-Agent: claude-code\n");
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
