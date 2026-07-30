use argon2::Argon2;
use zeroize::ZeroizeOnDrop;

#[derive(Clone, Debug)]
pub struct KdfParams {
    pub salt: Vec<u8>,
    pub time_cost: u32,
    pub memory_cost: u32,
    pub parallelism: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            salt: Vec::new(),
            time_cost: 3,
            memory_cost: 65536,
            parallelism: 4,
        }
    }
}

#[derive(ZeroizeOnDrop)]
pub struct DerivedKey {
    pub key: [u8; 32],
}

pub fn derive_kek(passphrase: &str, params: &KdfParams) -> Result<DerivedKey, String> {
    let argon = Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon2::Params::new(
            params.memory_cost,
            params.time_cost,
            params.parallelism,
            Some(32),
        )
        .map_err(|e| format!("Argon2 params: {e}"))?,
    );

    let mut key = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), &params.salt, &mut key)
        .map_err(|e| format!("Argon2 hash: {e}"))?;

    Ok(DerivedKey { key })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_same_params() {
        let pw = "test-passphrase";
        let salt = b"0123456789abcdef";
        let params = KdfParams {
            salt: salt.to_vec(),
            ..Default::default()
        };

        let k1 = derive_kek(pw, &params).unwrap();
        let k2 = derive_kek(pw, &params).unwrap();
        assert_eq!(k1.key, k2.key);
    }

    #[test]
    fn different_passphrase_different_key() {
        let salt = b"0123456789abcdef";
        let params = KdfParams {
            salt: salt.to_vec(),
            ..Default::default()
        };

        let k1 = derive_kek("password-a", &params).unwrap();
        let k2 = derive_kek("password-b", &params).unwrap();
        assert_ne!(k1.key, k2.key);
    }
}
