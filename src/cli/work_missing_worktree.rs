//! Missing-worktree diagnostics when claim succeeds but provisioning fails.
//! Split out of `work.rs` for the LOC gate.

/// Builds the user-facing message when `item::claim` returned `acquired` but
/// no `worktree_path`. The server already diagnoses most failures as
/// `worktree_error`; only fall back to the generic guess when it didn't.
pub(crate) fn missing_worktree_message(claim: &serde_json::Value) -> String {
    if let Some(error) = claim["worktree_error"].as_str() {
        if error.is_empty() {
            "claim succeeded but no worktree was created (bad git state?)".to_string()
        } else {
            format!("claim succeeded but no worktree was created: {error}")
        }
    } else {
        "claim succeeded but no worktree was created (bad git state?)".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_worktree_message_surfaces_server_worktree_error() {
        let claim = serde_json::json!({
            "worktree_error": "fatal: not a git repository (or any of the parent directories): .git"
        });
        let msg = missing_worktree_message(&claim);
        assert!(msg.contains("not a git repository"));
        assert!(!msg.ends_with("(bad git state?)"));
    }

    #[test]
    fn missing_worktree_message_falls_back_to_generic_guess_without_worktree_error() {
        let claim = serde_json::json!({ "status": "acquired" });
        assert_eq!(
            missing_worktree_message(&claim),
            "claim succeeded but no worktree was created (bad git state?)"
        );
    }
}
