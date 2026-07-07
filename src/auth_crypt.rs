use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use pbkdf2::pbkdf2_hmac_array;
use rand::RngCore;
use sha2::Sha256;
use std::io::Write;

const SALT: &[u8] = b"agentflare-vault-salt-v1";
const NONCE_SIZE: usize = 12;

pub fn get_passphrase() -> Option<String> {
    if let Ok(pw) = std::env::var("AGENTFLARE_VAULT_PASSPHRASE") {
        if !pw.is_empty() {
            return Some(pw);
        }
    }
    prompt_passphrase()
}

fn prompt_passphrase() -> Option<String> {
    print!("vault passphrase: ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok()?;
    let pw = input.trim().to_string();
    if pw.is_empty() { None } else { Some(pw) }
}

fn derive_key(passphrase: &str) -> [u8; 32] {
    pbkdf2_hmac_array::<Sha256, 32>(passphrase.as_bytes(), SALT, 100_000)
}

pub fn encrypt(plaintext: &[u8], passphrase: &str) -> Option<Vec<u8>> {
    let key = derive_key(passphrase);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher.encrypt(nonce, plaintext).ok()?;
    // Prepend nonce to ciphertext
    let mut result = nonce_bytes.to_vec();
    result.extend(ciphertext);
    Some(result)
}

pub fn decrypt(data: &[u8], passphrase: &str) -> Option<Vec<u8>> {
    if data.len() < NONCE_SIZE + 16 {
        return None; // too short for nonce + auth tag
    }
    let (nonce_bytes, ciphertext) = data.split_at(NONCE_SIZE);
    let key = derive_key(passphrase);
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
        let decrypted = decrypt(&encrypted, pw).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_passphrase_fails() {
        let encrypted = encrypt(b"secret", "correct").unwrap();
        assert!(decrypt(&encrypted, "wrong").is_none());
    }

    #[test]
    fn different_ciphertexts_for_same_input() {
        let c1 = encrypt(b"data", "pw").unwrap();
        let c2 = encrypt(b"data", "pw").unwrap();
        assert_ne!(c1, c2); // different nonces
    }
}
