mod keyring_cache;

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[cfg(test)]
pub use keyring_cache::{test_support, with_keyring_disabled, with_keyring_env};

// Session persistence is keyring-only by design (item #603): once
// `store_session` caches a DEK, the vault stays unsealed until an explicit
// `clear_session` call, same as `age`/`sops`-style tools. There is deliberately
// NO file fallback and NO TTL: any persistent cache decryptable without the OS
// secret store is breakable without it (the old XOR-with-username file proved
// exactly that), and callers already degrade to a passphrase prompt when
// `load_session` misses — see `open_vault_with_passphrase` / `inject.rs`'s
// `Locked` arm. If an idle timeout is ever needed, add an expiry the keyring
// entry itself enforces rather than a second on-disk copy.

fn vault_path_hash(vault_path: &Path) -> String {
    let hash = Sha256::digest(vault_path.to_string_lossy().as_bytes());
    hex::encode(&hash[..8])
}

/// Path of the pre-#603 XOR-obfuscated session file. Legacy only: current code
/// never writes here — this exists so `load_session`/`clear_session` can
/// delete-on-sight files left behind by older versions.
fn legacy_session_path(app_name: &str, entry_key: &str) -> PathBuf {
    crate::paths::home_dir()
        .join(format!(".{app_name}"))
        .join("cache")
        .join("vault-sessions")
        .join(format!("{entry_key}.session"))
}

/// Deletes a legacy XOR session file if one exists. Its contents are never
/// read: the format is keyed by public values (username, home, hostname) and
/// therefore untrustworthy by construction.
fn remove_legacy_file_cache(app_name: &str, entry_key: &str) {
    let path = legacy_session_path(app_name, entry_key);
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
}

pub fn load_session(app_name: &str, vault_path: &Path) -> Option<[u8; 32]> {
    let entry_key = vault_path_hash(vault_path);

    // Legacy XOR file, if any: delete on sight, never trust — regardless of
    // whether the keyring lookup below hits or misses.
    remove_legacy_file_cache(app_name, &entry_key);

    keyring_cache::load_from_keyring(app_name, &entry_key)
}

pub fn store_session(app_name: &str, vault_path: &Path, dek: &[u8; 32]) {
    let entry_key = vault_path_hash(vault_path);

    // Make sure no weak legacy file lingers, regardless of whether the
    // keyring store below succeeds or fails closed.
    remove_legacy_file_cache(app_name, &entry_key);

    // Result ignored by design: on failure we fail closed (memory-only for
    // this process). No file fallback — see the module doc comment above.
    let _ = keyring_cache::store_in_keyring(app_name, &entry_key, dek);
}

pub fn clear_session(app_name: &str, vault_path: &Path) {
    let entry_key = vault_path_hash(vault_path);
    keyring_cache::clear_keyring(app_name, &entry_key);
    remove_legacy_file_cache(app_name, &entry_key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;
    use keyring_cache::{with_keyring_disabled, with_keyring_env};
    use tempfile::TempDir;
    use test_support::use_shared_mock_keyring;

    fn legacy_path_for(app: &str, vault_path: &Path) -> PathBuf {
        legacy_session_path(app, &vault_path_hash(vault_path))
    }

    #[test]
    fn store_and_load_session() {
        with_keyring_env(|| {
            use_shared_mock_keyring();
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
        });
    }

    #[test]
    fn store_writes_no_file_when_keyring_is_available() {
        with_keyring_env(|| {
            use_shared_mock_keyring();
            with_temp_home(|| {
                let app = "flare-vault-test";
                let dir = TempDir::new().unwrap();
                let path = dir.path().join("vault.json");
                let dek = [0x42u8; 32];

                store_session(app, &path, &dek);
                assert!(
                    !legacy_path_for(app, &path).exists(),
                    "with a working keyring, no *.session file may be written"
                );
                keyring_cache::clear_keyring(app, &vault_path_hash(&path));
            });
        });
    }

    #[test]
    fn store_writes_no_file_and_load_misses_when_keyring_is_unavailable() {
        with_keyring_disabled(|| {
            with_temp_home(|| {
                let app = "flare-vault-test";
                let dir = TempDir::new().unwrap();
                let path = dir.path().join("vault.json");
                let dek = [0x42u8; 32];

                store_session(app, &path, &dek);
                assert!(
                    !legacy_path_for(app, &path).exists(),
                    "without a keyring the session must stay memory-only"
                );
                assert!(
                    load_session(app, &path).is_none(),
                    "callers fall back to a passphrase prompt (see unseal)"
                );
            });
        });
    }

    #[test]
    fn legacy_xor_file_is_ignored_and_deleted() {
        with_keyring_env(|| {
            use_shared_mock_keyring();
            with_temp_home(|| {
                let app = "flare-vault-test";
                let dir = TempDir::new().unwrap();
                let path = dir.path().join("vault.json");

                // A plausibly well-formed legacy file (valid length, arbitrary
                // bytes — the format is untrustworthy by construction, so the
                // exact contents must not matter).
                let legacy = legacy_path_for(app, &path);
                std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
                std::fs::write(&legacy, [0xABu8; 32]).unwrap();

                assert!(
                    load_session(app, &path).is_none(),
                    "a legacy XOR file must never authenticate a session"
                );
                assert!(
                    !legacy.exists(),
                    "a legacy XOR file must be deleted on sight"
                );
            });
        });
    }

    #[test]
    fn test_clear_session() {
        with_keyring_env(|| {
            use_shared_mock_keyring();
            with_temp_home(|| {
                let app = "flare-vault-test";
                let dir = TempDir::new().unwrap();
                let path = dir.path().join("vault.json");
                let dek = [0x42u8; 32];

                // Clear must also sweep a legacy file left by an older version.
                let legacy = legacy_path_for(app, &path);
                std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
                std::fs::write(&legacy, [0xABu8; 32]).unwrap();

                store_session(app, &path, &dek);
                super::clear_session(app, &path);
                assert!(!legacy.exists());
                assert!(
                    load_session(app, &path).is_none(),
                    "cleared sessions (and keyring-less stores) must not reload"
                );
            });
        });
    }
}
