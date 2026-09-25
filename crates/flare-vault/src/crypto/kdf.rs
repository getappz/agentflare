use argon2::Argon2;
use zeroize::ZeroizeOnDrop;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub salt: Vec<u8>,
    pub time_cost: u32,
    pub memory_cost: u32,
    pub parallelism: u32,
}

impl KdfParams {
    /// Argon2id iterations used for real vaults.
    pub const PRODUCTION_TIME_COST: u32 = 3;
    /// Argon2id memory in KiB used for real vaults (64 MiB).
    pub const PRODUCTION_MEMORY_COST: u32 = 65536;
    /// Argon2id lanes used for real vaults.
    pub const PRODUCTION_PARALLELISM: u32 = 4;

    /// The parameters every non-test build derives keys with.
    pub fn production() -> Self {
        Self {
            salt: Vec::new(),
            time_cost: Self::PRODUCTION_TIME_COST,
            memory_cost: Self::PRODUCTION_MEMORY_COST,
            parallelism: Self::PRODUCTION_PARALLELISM,
        }
    }

    /// Cheapest parameters Argon2 accepts. Test-only: a vault created with
    /// these is trivially brute-forceable.
    #[cfg(any(test, feature = "insecure-fast-kdf"))]
    pub fn insecure_fast() -> Self {
        Self {
            salt: Vec::new(),
            time_cost: 1,
            memory_cost: 16,
            parallelism: 1,
        }
    }
}

/// Production parameters, except in test builds (`cfg(test)` or the
/// `insecure-fast-kdf` feature) where the cheap parameters are used so vault
/// tests don't spend seconds per key.
/// The vault file stores only the salt, so `create_vault` and `open_vault`
/// both go through this impl and always agree within one build.
impl Default for KdfParams {
    fn default() -> Self {
        #[cfg(any(test, feature = "insecure-fast-kdf"))]
        {
            Self::insecure_fast()
        }
        #[cfg(not(any(test, feature = "insecure-fast-kdf")))]
        {
            Self::production()
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
    fn production_params_are_pinned() {
        // Deliberate change only: these define the cost of brute-forcing
        // every existing vault. Bump them together with a vault re-key path.
        let p = KdfParams::production();
        assert_eq!(p.time_cost, 3);
        assert_eq!(p.memory_cost, 65536);
        assert_eq!(p.parallelism, 4);
        assert!(p.salt.is_empty());
    }

    #[test]
    fn test_builds_default_to_fast_params() {
        assert_eq!(KdfParams::default(), KdfParams::insecure_fast());
        assert_ne!(KdfParams::default(), KdfParams::production());
    }

    #[test]
    fn fast_params_are_accepted_by_argon2() {
        let params = KdfParams {
            salt: b"0123456789abcdef".to_vec(),
            ..KdfParams::insecure_fast()
        };
        derive_kek("pw", &params).unwrap();
    }

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
