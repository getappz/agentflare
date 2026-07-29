mod file_cache;
mod keyring_cache;

use sha2::{Digest, Sha256};
use std::path::Path;

// No TTL here by design -- once `store_session` caches a DEK, the vault
// stays unsealed (via keyring or the file-cache fallback) until an explicit
// `clear_session` call, same as `age`/`sops`-style tools. If this needs an
// idle timeout later, add an expiry timestamp alongside the cached DEK
// rather than relying on callers to lock() proactively.

fn vault_path_hash(vault_path: &Path) -> String {
    let hash = Sha256::digest(vault_path.to_string_lossy().as_bytes());
    hex::encode(&hash[..8])
}

pub fn load_session(app_name: &str, vault_path: &Path) -> Option<[u8; 32]> {
    let entry_key = vault_path_hash(vault_path);

    // Try keyring first
    if let Some(dek) = keyring_cache::load_from_keyring(app_name, &entry_key) {
        return Some(dek);
    }

    // Fall back to file cache
    file_cache::load_from_file_cache(app_name, &entry_key)
}

pub fn store_session(app_name: &str, vault_path: &Path, dek: &[u8; 32]) {
    let entry_key = vault_path_hash(vault_path);

    keyring_cache::store_in_keyring(app_name, &entry_key, dek);
    file_cache::store_in_file_cache(app_name, &entry_key, dek);
}

pub fn clear_session(app_name: &str, vault_path: &Path) {
    let entry_key = vault_path_hash(vault_path);
    keyring_cache::clear_keyring(app_name, &entry_key);
    file_cache::clear_file_cache(app_name, &entry_key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;
    use tempfile::TempDir;

    #[test]
    fn store_and_load_session() {
        with_temp_home(|| {
            let app = "flare-vault-test";
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("vault.json");
            let dek = [0x42u8; 32];

            store_session(app, &path, &dek);
            let loaded = load_session(app, &path);
            assert!(loaded.is_some());
            assert_eq!(loaded.unwrap(), dek);
        });
    }

    #[test]
    fn test_clear_session() {
        with_temp_home(|| {
            let app = "flare-vault-test";
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("vault.json");
            let dek = [0x42u8; 32];

            store_session(app, &path, &dek);
            super::clear_session(app, &path);
            let loaded = load_session(app, &path);
            assert!(loaded.is_none());
        });
    }
}
