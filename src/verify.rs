//! `[verify]` config (`.agentflare/config.toml`): opt-in completion
//! verification. `enforce` defaults to false. The per-item
//! `metadata.require_verify = true` override is read by the completion gate
//! (follow-on item), not here.

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
}
