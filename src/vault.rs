#![allow(dead_code)]

use flare_vault::inject::load_vault_env;
use flare_vault::vault::manager::{
    VaultDek, create_vault, get_secret_value, list_secret_names, open_vault, open_vault_with_dek,
    read_vault_body, remove_secret_value, set_secret_value, write_vault_body,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

const APP_NAME: &str = "agentflare";

static PASSPHRASE_CACHE: Mutex<Option<String>> = Mutex::new(None);

pub fn get_passphrase() -> Option<String> {
    if let Ok(pw) = std::env::var("AGENTFLARE_VAULT_PASSPHRASE")
        && !pw.is_empty()
    {
        return Some(pw);
    }
    PASSPHRASE_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub fn cache_passphrase(passphrase: &str) {
    if let Ok(mut cache) = PASSPHRASE_CACHE.lock() {
        *cache = Some(passphrase.to_string());
    }
}

fn vault_path() -> std::path::PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    home.join(".agentflare").join("vault.json")
}

pub fn ensure_vault() -> Result<(), String> {
    let path = vault_path();
    if path.exists() {
        return Ok(());
    }
    let pw = get_passphrase().ok_or_else(|| {
        "no vault passphrase available (set AGENTFLARE_VAULT_PASSPHRASE or run interactively)"
            .to_string()
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    create_vault(&path, &pw).map_err(|e| e.to_string())?;
    Ok(())
}

fn open_vault_with_passphrase() -> Result<VaultDek, String> {
    let path = vault_path();
    let pw = get_passphrase().ok_or_else(|| {
        "no vault passphrase available (set AGENTFLARE_VAULT_PASSPHRASE or run interactively)"
            .to_string()
    })?;
    let dek = open_vault(&path, &pw).map_err(|e| e.to_string())?;
    migrate_legacy_secrets(&path, &dek, &pw);
    Ok(dek)
}

/// One-time, best-effort import of secrets from the pre-vault
/// `gateway_secrets` sqlite table (see src/gateway_secrets.rs) into the new
/// vault, so upgrading doesn't silently drop already-configured secrets
/// (e.g. `github_token`, `telegram_bot_token`). Runs at most once per
/// process, only fills in names the vault doesn't already have, and never
/// fails the caller — migration failures just leave the legacy value in
/// place for a future attempt.
fn migrate_legacy_secrets(path: &Path, dek: &VaultDek, passphrase: &str) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }

    let Ok(conn) = crate::db::open() else { return };
    let Ok(names) = crate::gateway_secrets::list_secrets(&conn) else {
        return;
    };
    if names.is_empty() {
        return;
    }
    let Ok(mut body) = read_vault_body(path) else {
        return;
    };

    let mut changed = false;
    for name in &names {
        if body.contains_key(name) {
            continue;
        }
        let ciphertext: Option<Vec<u8>> = conn
            .query_row(
                "SELECT ciphertext FROM gateway_secrets WHERE name = ?1",
                rusqlite::params![name],
                |r| r.get(0),
            )
            .ok();
        let Some(value) = ciphertext
            .as_deref()
            .and_then(|ct| crate::auth_crypt::decrypt(ct, passphrase))
            .and_then(|bytes| String::from_utf8(bytes).ok())
        else {
            continue;
        };
        if set_secret_value(&mut body, &dek.dek, name, &value).is_ok() {
            changed = true;
        }
    }
    if changed {
        let _ = write_vault_body(path, &body);
    }
}

pub fn vault_file_path() -> std::path::PathBuf {
    vault_path()
}

pub fn unlock(passphrase: &str) -> Result<(), String> {
    let path = vault_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    if !path.exists() {
        create_vault(&path, passphrase).map_err(|e| e.to_string())?;
    }
    let dek = open_vault(&path, passphrase).map_err(|e| e.to_string())?;
    migrate_legacy_secrets(&path, &dek, passphrase);
    flare_vault::session::store_session(APP_NAME, &path, &dek.dek);
    cache_passphrase(passphrase);
    Ok(())
}

pub fn lock() -> Result<(), String> {
    let path = vault_path();
    flare_vault::session::clear_session(APP_NAME, &path);
    if let Ok(mut cache) = PASSPHRASE_CACHE.lock() {
        *cache = None;
    }
    Ok(())
}

fn unseal_vault_with_dek() -> Result<VaultDek, String> {
    let path = vault_path();
    ensure_vault()?;

    if let Ok(dek) = open_vault_with_dek(&path, APP_NAME) {
        return Ok(dek);
    }

    open_vault_with_passphrase()
}

pub fn get_secret(name: &str) -> Result<Option<String>, String> {
    let path = vault_path();
    if !path.exists() {
        return Ok(None);
    }
    let dek = unseal_vault_with_dek()?;
    let body = read_vault_body(&path).map_err(|e| e.to_string())?;
    get_secret_value(&body, &dek.dek, name).map_err(|e| e.to_string())
}

pub fn set_secret(name: &str, value: &str) -> Result<(), String> {
    let dek = unseal_vault_with_dek()?;
    let path = vault_path();
    let mut body = read_vault_body(&path).map_err(|e| e.to_string())?;
    set_secret_value(&mut body, &dek.dek, name, value).map_err(|e| e.to_string())?;
    write_vault_body(&path, &body).map_err(|e| e.to_string())
}

pub fn list_secrets() -> Result<Vec<String>, String> {
    let path = vault_path();
    let body = read_vault_body(&path).map_err(|e| e.to_string())?;
    Ok(list_secret_names(&body))
}

pub fn remove_secret(name: &str) -> Result<bool, String> {
    let _dek = unseal_vault_with_dek()?;
    let path = vault_path();
    let mut body = read_vault_body(&path).map_err(|e| e.to_string())?;
    let removed = remove_secret_value(&mut body, name).map_err(|e| e.to_string())?;
    write_vault_body(&path, &body).map_err(|e| e.to_string())?;
    Ok(removed)
}

pub fn vault_env(working_dir: &Path) -> HashMap<String, String> {
    load_vault_env(APP_NAME, working_dir).unwrap_or_default()
}
