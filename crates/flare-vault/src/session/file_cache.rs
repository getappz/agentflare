use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn cache_dir(app_name: &str) -> PathBuf {
    crate::paths::home_dir()
        .join(format!(".{app_name}"))
        .join("cache")
        .join("vault-sessions")
}

fn session_path(app_name: &str, entry_key: &str) -> PathBuf {
    cache_dir(app_name).join(format!("{entry_key}.session"))
}

/// Derive a local encryption key from user+home+hostname.
/// Not a security boundary — blocks casual file access.
fn derive_cache_key(app_name: &str) -> Vec<u8> {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into());
    let home = crate::paths::home_dir().display().to_string();
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown".into());

    let mut hasher = Sha256::new();
    hasher.update(app_name.as_bytes());
    hasher.update(user.as_bytes());
    hasher.update(home.as_bytes());
    hasher.update(hostname.as_bytes());
    hasher.finalize().to_vec()
}

pub fn load_from_file_cache(app_name: &str, entry_key: &str) -> Option<[u8; 32]> {
    let path = session_path(app_name, entry_key);
    let data = std::fs::read(&path).ok()?;

    // Simple XOR obfuscation with cache key
    let cache_key = derive_cache_key(app_name);
    let dek = xor_decrypt(&data, &cache_key);
    if dek.len() != 32 {
        return None;
    }
    let mut result = [0u8; 32];
    result.copy_from_slice(&dek);
    Some(result)
}

pub fn store_in_file_cache(app_name: &str, entry_key: &str, dek: &[u8; 32]) {
    let path = session_path(app_name, entry_key);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cache_key = derive_cache_key(app_name);
    let encrypted = xor_encrypt(dek, &cache_key);
    let _ = std::fs::write(&path, encrypted);
}

pub fn clear_file_cache(app_name: &str, entry_key: &str) {
    let path = session_path(app_name, entry_key);
    let _ = std::fs::remove_file(&path);
}

fn xor_encrypt(data: &[u8], key: &[u8]) -> Vec<u8> {
    data.iter()
        .enumerate()
        .map(|(i, b)| b ^ key[i % key.len()])
        .collect()
}

fn xor_decrypt(data: &[u8], key: &[u8]) -> Vec<u8> {
    xor_encrypt(data, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn store_and_load() {
        with_temp_home(|| {
            let app = "flare-vault-test";
            let entry_key = "test-key";
            let dek = [0xABu8; 32];

            store_in_file_cache(app, entry_key, &dek);
            let loaded = load_from_file_cache(app, entry_key);
            assert!(loaded.is_some());
            assert_eq!(loaded.unwrap(), dek);
        });
    }

    #[test]
    fn clear() {
        with_temp_home(|| {
            let app = "flare-vault-test";
            let entry_key = "test-key";
            let dek = [0xABu8; 32];

            store_in_file_cache(app, entry_key, &dek);
            clear_file_cache(app, entry_key);
            assert!(load_from_file_cache(app, entry_key).is_none());
        });
    }
}
