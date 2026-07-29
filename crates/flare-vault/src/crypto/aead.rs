use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::RngCore;
pub const MAGIC: &[u8] = b"FLVT";
pub const NONCE_SIZE: usize = 12;

pub struct EncryptedBlob {
    pub magic: [u8; 4],
    pub nonce: [u8; 12],
    pub ciphertext: Vec<u8>,
}

impl EncryptedBlob {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(self.magic.len() + self.nonce.len() + self.ciphertext.len());
        out.extend_from_slice(&self.magic);
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&self.ciphertext);
        out
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, String> {
        if data.len() < MAGIC.len() + NONCE_SIZE {
            return Err("encrypted blob too short".into());
        }
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&data[..MAGIC.len()]);
        let mut nonce = [0u8; NONCE_SIZE];
        nonce.copy_from_slice(&data[MAGIC.len()..MAGIC.len() + NONCE_SIZE]);
        let ciphertext = data[MAGIC.len() + NONCE_SIZE..].to_vec();
        Ok(Self {
            magic,
            nonce,
            ciphertext,
        })
    }
}

pub fn encrypt_dek(plaintext: &[u8], kek: &[u8; 32]) -> Result<EncryptedBlob, String> {
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(kek));
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| format!("AES-GCM encrypt: {e}"))?;
    Ok(EncryptedBlob {
        magic: *b"FLVT",
        nonce: nonce_bytes,
        ciphertext,
    })
}

pub fn decrypt_dek(blob: &EncryptedBlob, kek: &[u8; 32]) -> Result<Vec<u8>, String> {
    if &blob.magic != b"FLVT" {
        return Err("invalid magic".into());
    }
    let nonce = Nonce::from_slice(&blob.nonce);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(kek));
    cipher
        .decrypt(nonce, blob.ciphertext.as_slice())
        .map_err(|_| "AES-GCM decrypt failed (wrong key or corrupt)".into())
}

pub fn encrypt_value(plaintext: &[u8], dek: &[u8; 32]) -> Result<Vec<u8>, String> {
    let mut nonce_bytes = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(dek));
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| format!("AES-GCM encrypt value: {e}"))?;
    Ok(EncryptedBlob {
        magic: *b"FLVT",
        nonce: nonce_bytes,
        ciphertext,
    }
    .to_bytes())
}

pub fn decrypt_value(data: &[u8], dek: &[u8; 32]) -> Result<Vec<u8>, String> {
    let blob = EncryptedBlob::from_bytes(data)?;
    if blob.magic != MAGIC {
        return Err("invalid magic".into());
    }
    let nonce = Nonce::from_slice(&blob.nonce);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(dek));
    cipher
        .decrypt(nonce, blob.ciphertext.as_slice())
        .map_err(|_| "AES-GCM decrypt failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dek_roundtrip() {
        let kek = [0xABu8; 32];
        let dek = b"this-is-a-256-bit-key-dummy!";
        let blob = encrypt_dek(dek, &kek).unwrap();
        let decrypted = decrypt_dek(&blob, &kek).unwrap();
        assert_eq!(decrypted, dek);
    }

    #[test]
    fn dek_wrong_kek_fails() {
        let kek1 = [0xABu8; 32];
        let kek2 = [0xBAu8; 32];
        let blob = encrypt_dek(b"my-dek-value", &kek1).unwrap();
        assert!(decrypt_dek(&blob, &kek2).is_err());
    }

    #[test]
    fn value_roundtrip() {
        let dek = [0x42u8; 32];
        let plaintext = b"my-secret-api-key-12345";
        let encrypted = encrypt_value(plaintext, &dek).unwrap();
        let decrypted = decrypt_value(&encrypted, &dek).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn unique_nonces() {
        let dek = [0x42u8; 32];
        let plaintext = b"same-data";
        let c1 = encrypt_value(plaintext, &dek).unwrap();
        let c2 = encrypt_value(plaintext, &dek).unwrap();
        assert_ne!(c1, c2);
    }

    #[test]
    fn wrong_dek_fails() {
        let dek1 = [0x42u8; 32];
        let dek2 = [0x24u8; 32];
        let encrypted = encrypt_value(b"secret", &dek1).unwrap();
        assert!(decrypt_value(&encrypted, &dek2).is_err());
    }
}
