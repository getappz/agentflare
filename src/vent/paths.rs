use std::path::PathBuf;

pub fn repo_key() -> String {
    if let Ok(out) = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        && out.status.success()
    {
        let remote = String::from_utf8_lossy(&out.stdout);
        let remote = remote.trim();
        if !remote.is_empty() {
            return format!("git:{}", crate::claims::normalize_repo(remote));
        }
    }
    let root = repo_root();
    let canonical = std::fs::canonicalize(&root).unwrap_or(root);
    format!("path:{}", canonical.to_string_lossy())
}

/// Same resolution `AgentflareMcp` uses for `.agentflare/project.json` —
/// git toplevel, else a marker-based walk-up for non-git projects. Vent must
/// resolve to the identical root or `consolidate()` looks for the project
/// link in the wrong directory.
pub fn repo_root() -> PathBuf {
    crate::mcp_server::AgentflareMcp::repo_root()
}

fn repo_slug() -> String {
    repo_key()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

pub fn log_path() -> PathBuf {
    crate::state::state_dir()
        .join("vents")
        .join(format!("{}.jsonl", repo_slug()))
}

pub fn cursor_path() -> PathBuf {
    crate::state::state_dir()
        .join("vents")
        .join(format!("{}.cursor", repo_slug()))
}

/// Global, cross-repo log for agentflare-core vents (git shim / af-guard /
/// item-tracker friction) — deliberately NOT repo-slugged like `log_path()`,
/// so the same underlying agentflare bug hit in different repos dedups into
/// one place instead of splitting across N per-project rows.
pub fn global_log_path() -> PathBuf {
    crate::state::state_dir()
        .join("vents")
        .join("agentflare-core.jsonl")
}

/// `topic_key -> {issue_url, filed_at}` for the global agentflare-core log.
/// A vent is either filed or it isn't — no separate cursor file is needed.
pub fn global_filed_path() -> PathBuf {
    crate::state::state_dir()
        .join("vents")
        .join("agentflare-core.filed.json")
}

pub fn backend_db_path() -> PathBuf {
    crate::paths::home().join(".agentflare").join("backend.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_and_cursor_are_siblings_under_state_dir() {
        // repo_key() reads cwd (via repo_root()) and AGENTFLARE_HOME_OVERRIDE,
        // both process-global and mutated by with_temp_cwd/with_temp_home
        // elsewhere in this binary -- without this lock, a concurrent test can
        // change either between the two repo_key() calls below (log_path()
        // then cursor_path()), producing mismatched slugs. Flaked on Linux CI
        // under higher default test parallelism; not reliably reproduced locally.
        let _guard = agent_registry::detect::PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let log = log_path();
        let cur = cursor_path();
        assert!(log.to_string_lossy().contains("vents"));
        assert_eq!(cur.extension().unwrap(), "cursor");
        assert_eq!(log.parent(), cur.parent());
        assert_eq!(log.file_stem(), cur.file_stem());
    }

    #[test]
    fn repo_key_is_stable() {
        assert_eq!(repo_key(), repo_key());
    }

    #[test]
    fn global_paths_are_siblings_and_not_repo_scoped() {
        let _guard = agent_registry::detect::PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let log = global_log_path();
        let filed = global_filed_path();
        assert!(log.to_string_lossy().contains("agentflare-core.jsonl"));
        assert!(filed.to_string_lossy().contains("agentflare-core.filed.json"));
        assert_eq!(log.parent(), filed.parent());
        assert_eq!(global_log_path(), global_log_path());
    }
}
