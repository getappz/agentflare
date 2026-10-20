//! `[verify]` config (`.agentflare/config.toml`): opt-in completion
//! verification. `enforce` defaults to false; `metadata.require_verify`
//! forces it on for a single item.

use std::path::Path;

/// `[verify] enforce`, project-local layer winning over user-home; false
/// when unset or the config can't be read.
pub fn enforce_enabled(repo_root: &Path) -> bool {
    enforce_from(repo_root, &crate::paths::home())
}

fn enforce_from(repo_root: &Path, home: &Path) -> bool {
    let Ok(layers) = flare_git_core::config_loader::locate_and_parse(repo_root, Some(home)) else {
        return false;
    };
    [
        layers.user_home.as_ref().map(|(_, v)| v),
        layers.project_local.as_ref().map(|(_, v)| v),
    ]
    .into_iter()
    .flatten()
    .filter_map(|doc| doc.get("verify")?.get("enforce")?.as_bool())
    .last()
    .unwrap_or(false)
}

/// Whether verification applies to an item: project default or the item's
/// `metadata.require_verify = true`. No caller yet — the completion gate
/// (follow-on item) consumes it.
#[allow(dead_code)]
pub fn required_for(repo_root: &Path, item_metadata: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(item_metadata)
        .ok()
        .and_then(|m| m.get("require_verify")?.as_bool())
        .unwrap_or(false)
        || enforce_enabled(repo_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_cfg(root: &Path, body: &str) {
        std::fs::create_dir_all(root.join(".agentflare")).unwrap();
        std::fs::write(root.join(".agentflare").join("config.toml"), body).unwrap();
    }

    #[test]
    fn enforce_defaults_false_and_reads_true() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        assert!(!enforce_from(repo.path(), home.path()));
        write_cfg(repo.path(), "[verify]\nenforce = true\n");
        assert!(enforce_from(repo.path(), home.path()));
    }

    #[test]
    fn project_local_overrides_user_home() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write_cfg(home.path(), "[verify]\nenforce = true\n");
        assert!(enforce_from(repo.path(), home.path()));
        write_cfg(repo.path(), "[verify]\nenforce = false\n");
        assert!(!enforce_from(repo.path(), home.path()));
    }

    #[test]
    fn malformed_config_is_off() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write_cfg(repo.path(), "[verify\nenforce");
        assert!(!enforce_from(repo.path(), home.path()));
    }

    #[test]
    fn metadata_override_forces_on() {
        let dir = std::env::temp_dir().join("verify-test-no-config");
        assert!(required_for(&dir, r#"{"require_verify":true}"#));
        assert!(!required_for(&dir, "{}"));
        assert!(!required_for(&dir, "not json"));
    }
}
