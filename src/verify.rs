//! `[verify]` config (`.agentflare/config.toml`): opt-in completion
//! verification. `enforce` defaults to false; `metadata.require_verify`
//! forces it on for a single item.

use std::path::Path;

/// `[verify] enforce`, project-local layer winning over user-home; false
/// when unset or the config can't be read.
pub fn enforce_enabled(repo_root: &Path) -> bool {
    let Ok(layers) =
        flare_git_core::config_loader::locate_and_parse(repo_root, Some(&crate::paths::home()))
    else {
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
/// `metadata.require_verify = true`.
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

    #[test]
    fn metadata_override_forces_on() {
        let dir = std::env::temp_dir().join("verify-test-no-config");
        assert!(required_for(&dir, r#"{"require_verify":true}"#));
        assert!(!required_for(&dir, "{}"));
        assert!(!required_for(&dir, "not json"));
    }
}
