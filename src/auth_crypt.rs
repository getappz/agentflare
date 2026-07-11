use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use pbkdf2::pbkdf2_hmac_array;
use rand::RngCore;
use sha2::Sha256;

const MAGIC: &[u8] = b"AFVE";
const SALT_SIZE: usize = 16;
const NONCE_SIZE: usize = 12;
const ITERATIONS: u32 = 600_000;

pub fn get_passphrase() -> Option<String> {
    if let Ok(pw) = std::env::var("AGENTFLARE_VAULT_PASSPHRASE")
        && !pw.is_empty()
    {
        return Some(pw);
    }
    prompt_passphrase()
}

fn prompt_passphrase() -> Option<String> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return None;
    }
    let pw = rpassword::prompt_password("vault passphrase: ").unwrap_or_default();
    let trimmed = pw.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn derive_key(passphrase: &str, salt: &[u8]) -> [u8; 32] {
    pbkdf2_hmac_array::<Sha256, 32>(passphrase.as_bytes(), salt, ITERATIONS)
}

pub fn is_encrypted(data: &[u8]) -> bool {
    data.len() >= MAGIC.len() && &data[..MAGIC.len()] == MAGIC
}

pub fn encrypt(plaintext: &[u8], passphrase: &str) -> Option<Vec<u8>> {
    let mut salt = [0u8; SALT_SIZE];
    OsRng.fill_bytes(&mut salt);
    let key = derive_key(passphrase, &salt);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher.encrypt(nonce, plaintext).ok()?;
    let mut result = MAGIC.to_vec();
    result.extend_from_slice(&salt);
    result.extend_from_slice(&nonce_bytes);
    result.extend(ciphertext);
    Some(result)
}

pub fn decrypt(data: &[u8], passphrase: &str) -> Option<Vec<u8>> {
    if !is_encrypted(data) {
        return None;
    }
    // Format: MAGIC(4) || salt(16) || nonce(12) || ciphertext
    let payload = &data[MAGIC.len()..];
    if payload.len() < SALT_SIZE + NONCE_SIZE + 16 {
        return None;
    }
    let salt = &payload[..SALT_SIZE];
    let (nonce_bytes, ciphertext) = payload[SALT_SIZE..].split_at(NONCE_SIZE);
    let key = derive_key(passphrase, salt);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(nonce_bytes);
    cipher.decrypt(nonce, ciphertext).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let pw = "test-passphrase";
        let plaintext = b"hello world";
        let encrypted = encrypt(plaintext, pw).unwrap();
        assert!(is_encrypted(&encrypted));
        let decrypted = decrypt(&encrypted, pw).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_passphrase_fails() {
        let encrypted = encrypt(b"secret", "correct").unwrap();
        assert!(decrypt(&encrypted, "wrong").is_none());
    }

    #[test]
    fn different_salts_for_same_input() {
        let c1 = encrypt(b"data", "pw").unwrap();
        let c2 = encrypt(b"data", "pw").unwrap();
        assert_ne!(c1, c2);
    }

    #[test]
    fn is_encrypted_detects_magic() {
        let encrypted = encrypt(b"x", "pw").unwrap();
        assert!(is_encrypted(&encrypted));
        assert!(!is_encrypted(b"not encrypted"));
        assert!(!is_encrypted(b""));
    }
}
