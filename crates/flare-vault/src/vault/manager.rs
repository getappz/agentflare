use crate::crypto::aead::{decrypt_dek, decrypt_value, encrypt_dek, encrypt_value};
use crate::crypto::kdf::{derive_kek, KdfParams};
use crate::error::{VaultError, VaultResult};
use crate::session;
use crate::vault::file::{read_vault_file, vault_file_exists, write_vault_file};
use crate::vault::model::{SecretEntry, VaultBody, VaultFile};
use chrono::Utc;
use rand::RngCore;
use std::path::{Path, PathBuf};
use zeroize::{ZeroizeOnDrop, Zeroizing};

const SALT_SIZE: usize = 32;
const DEK_SIZE: usize = 32;

#[derive(ZeroizeOnDrop)]
pub struct VaultDek {
    pub dek: [u8; 32],
}

pub struct VaultPaths {
    pub global_dir: PathBuf,
    pub project_dir: Option<PathBuf>,
}

impl VaultPaths {
    pub fn global(app_name: &str) -> Self {
        let home = crate::paths::home_dir();
        Self {
            global_dir: home.join(format!(".{app_name}")),
            project_dir: None,
        }
    }

    pub fn project<F>(app_name: &str, start: &Path, is_project_root: F) -> Self
    where
        F: Fn(&Path) -> bool,
    {
        let home = crate::paths::home_dir();
        let global_dir = home.join(format!(".{app_name}"));
        let project_dir = resolve_project_dir(start, &home, is_project_root);
        Self {
            global_dir,
            project_dir,
        }
    }

    pub fn global_vault_path(&self) -> PathBuf {
        self.global_dir.join("vault.json")
    }

    pub fn project_vault_path(&self) -> Option<PathBuf> {
        self.project_dir.as_ref().map(|d| d.join("vault.json"))
    }
}

fn resolve_project_dir<F>(start: &Path, home: &Path, is_project_root: F) -> Option<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    let mut current = Some(start.to_path_buf());
    while let Some(dir) = current {
        if dir == home || dir.parent().is_none() {
            return None;
        }
        if is_project_root(&dir) {
            return Some(dir.join(".agentflare"));
        }
        current = dir.parent().map(|p| p.to_path_buf());
    }
    None
}

pub fn create_vault(path: &Path, passphrase: &str) -> VaultResult<()> {
    if vault_file_exists(path) {
        return Err(VaultError::AlreadyInitialized(path.display().to_string()));
    }

    let mut salt = vec![0u8; SALT_SIZE];
    rand::rngs::OsRng.fill_bytes(&mut salt);

    let params = KdfParams {
        salt: salt.clone(),
        ..Default::default()
    };

    let kek = derive_kek(passphrase, &params).map_err(VaultError::Crypto)?;

    let mut dek = [0u8; DEK_SIZE];
    rand::rngs::OsRng.fill_bytes(&mut dek);

    let blob = encrypt_dek(&dek, &kek.key).map_err(VaultError::Crypto)?;
    let vault = VaultFile::new(blob.to_bytes(), salt);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(&vault)?;
    write_vault_file(path, json.as_bytes())
}

pub fn open_vault(path: &Path, passphrase: &str) -> VaultResult<VaultDek> {
    let data = read_vault_file(path)?;
    let vault: VaultFile = serde_json::from_slice(&data)?;

    let params = KdfParams {
        salt: vault.salt,
        ..Default::default()
    };

    let kek = derive_kek(passphrase, &params).map_err(VaultError::Crypto)?;

    let blob = crate::crypto::aead::EncryptedBlob::from_bytes(&vault.encrypted_dek)
        .map_err(VaultError::Crypto)?;

    let dek_bytes = decrypt_dek(&blob, &kek.key).map_err(|_| VaultError::WrongPassphrase)?;
    let mut dek = [0u8; DEK_SIZE];
    dek.copy_from_slice(&dek_bytes);

    Ok(VaultDek { dek })
}

pub fn open_vault_with_dek(path: &Path, app_name: &str) -> VaultResult<VaultDek> {
    if let Some(dek) = session::load_session(app_name, path) {
        return Ok(VaultDek { dek });
    }
    Err(VaultError::Locked)
}

pub fn read_vault_body(path: &Path) -> VaultResult<VaultBody> {
    let data = read_vault_file(path)?;
    let vault: VaultFile = serde_json::from_slice(&data)?;
    Ok(vault.body)
}

pub fn write_vault_body(path: &Path, body: &VaultBody) -> VaultResult<()> {
    let data = read_vault_file(path)?;
    let mut vault: VaultFile = serde_json::from_slice(&data)?;
    vault.body = body.clone();
    let json = serde_json::to_string_pretty(&vault)?;
    write_vault_file(path, json.as_bytes())
}

/// Returns the decrypted secret wrapped in `Zeroizing` so the plaintext is
/// wiped from memory as soon as the caller is done with it, rather than
/// lingering in the heap for the rest of the process's life.
pub fn get_secret_value(
    body: &VaultBody,
    dek: &[u8; 32],
    name: &str,
) -> VaultResult<Option<Zeroizing<String>>> {
    let entry = match body.get(name) {
        Some(e) => e,
        None => return Ok(None),
    };
    let plaintext = decrypt_value(&entry.value, dek).map_err(|_| VaultError::WrongPassphrase)?;
    let s = String::from_utf8(plaintext)
        .map_err(|_| VaultError::Crypto("invalid UTF-8 in secret".into()))?;
    Ok(Some(Zeroizing::new(s)))
}

pub fn set_secret_value(
    body: &mut VaultBody,
    dek: &[u8; 32],
    name: &str,
    value: &str,
) -> VaultResult<()> {
    let encrypted = encrypt_value(value.as_bytes(), dek).map_err(VaultError::Crypto)?;
    body.insert(
        name.to_string(),
        SecretEntry {
            value: encrypted,
            added_at: Utc::now(),
            rotated_at: None,
        },
    );
    Ok(())
}

pub fn remove_secret_value(body: &mut VaultBody, name: &str) -> VaultResult<bool> {
    Ok(body.remove(name).is_some())
}

pub fn list_secret_names(body: &VaultBody) -> Vec<String> {
    body.keys().cloned().collect()
}

pub fn merge_secrets(base: &mut VaultBody, overlay: &VaultBody) {
    for (key, entry) in overlay.iter() {
        if !base.contains_key(key) {
            base.insert(key.clone(), entry.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn create_and_open_vault() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.json");
        create_vault(&path, "test-pw").unwrap();
        let dek = open_vault(&path, "test-pw").unwrap();
        assert_eq!(dek.dek.len(), 32);
    }

    #[test]
    fn wrong_passphrase_fails() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.json");
        create_vault(&path, "correct").unwrap();
        let err = open_vault(&path, "wrong");
        assert!(matches!(err, Err(VaultError::WrongPassphrase)));
    }

    #[test]
    fn double_create_fails() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.json");
        create_vault(&path, "pw").unwrap();
        let err = create_vault(&path, "pw");
        assert!(matches!(err, Err(VaultError::AlreadyInitialized(_))));
    }

    #[test]
    fn set_get_secret() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.json");
        create_vault(&path, "pw").unwrap();
        let dek = open_vault(&path, "pw").unwrap();
        let mut body = VaultBody::default();
        set_secret_value(&mut body, &dek.dek, "api_key", "sk-12345").unwrap();
        let val = get_secret_value(&body, &dek.dek, "api_key").unwrap();
        assert_eq!(val.unwrap().as_str(), "sk-12345");
    }

    #[test]
    fn remove_and_list() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.json");
        create_vault(&path, "pw").unwrap();
        let dek = open_vault(&path, "pw").unwrap();
        let mut body = VaultBody::default();
        set_secret_value(&mut body, &dek.dek, "a", "1").unwrap();
        set_secret_value(&mut body, &dek.dek, "b", "2").unwrap();
        let names = list_secret_names(&body);
        assert_eq!(names.len(), 2);
        assert!(remove_secret_value(&mut body, "a").unwrap());
        assert!(!remove_secret_value(&mut body, "a").unwrap());
    }

    #[test]
    fn save_and_reopen_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("vault.json");
        create_vault(&path, "pw").unwrap();
        let dek = open_vault(&path, "pw").unwrap();
        let mut body = VaultBody::default();
        set_secret_value(&mut body, &dek.dek, "key", "value").unwrap();
        write_vault_body(&path, &body).unwrap();

        let body2 = read_vault_body(&path).unwrap();
        let val = get_secret_value(&body2, &dek.dek, "key").unwrap();
        assert_eq!(val.unwrap().as_str(), "value");
    }

    #[test]
    fn merge_secrets_does_not_overwrite() {
        let mut base = VaultBody::default();
        let mut overlay = VaultBody::default();
        base.insert(
            "shared".into(),
            SecretEntry {
                value: b"base-value".to_vec(),
                added_at: Utc::now(),
                rotated_at: None,
            },
        );
        overlay.insert(
            "shared".into(),
            SecretEntry {
                value: b"overlay-value".to_vec(),
                added_at: Utc::now(),
                rotated_at: None,
            },
        );
        merge_secrets(&mut base, &overlay);
        assert_eq!(base.get("shared").unwrap().value, b"base-value".to_vec());
    }
}
