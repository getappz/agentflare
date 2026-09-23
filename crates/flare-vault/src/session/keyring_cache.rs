use base64::Engine;

/// Serializes every read/mutation of the `FLARE_VAULT_KEYRING` escape hatch
/// below: env vars are process-global, so parallel tests forcing the
/// no-keyring path must hold this while the tests merely *reading* the value
/// hold it too (see `with_keyring_env`/`with_keyring_disabled`). Mirrors
/// `paths::test_support`'s `HOME_OVERRIDE_LOCK` discipline for the same
/// reason. Zero production impact — prod code never touches this lock.
#[cfg(test)]
static KEYRING_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Escape hatch for headless environments with no OS secret store
/// (`FLARE_VAULT_KEYRING=off` forces the no-keyring path: sessions stay
/// memory-only for the process; callers fall back to a passphrase prompt).
/// Anything else (including unset) means "use the keyring".
fn keyring_enabled() -> bool {
    std::env::var("FLARE_VAULT_KEYRING")
        .map(|v| v != "off")
        .unwrap_or(true)
}

/// Runs `f` with the keyring forced available (clears any ambient
/// `FLARE_VAULT_KEYRING=off`) and serialized (see `KEYRING_ENV_LOCK`).
/// Test-only seam: production resolves availability inline and never calls
/// this.
#[cfg(test)]
pub fn with_keyring_env<R>(f: impl FnOnce() -> R) -> R {
    struct ResetOnDrop(Option<String>);
    impl Drop for ResetOnDrop {
        fn drop(&mut self) {
            // SAFETY: KEYRING_ENV_LOCK is held for the whole closure (see
            // below); no other thread can read or write this var concurrently.
            unsafe {
                match self.0.take() {
                    Some(v) => std::env::set_var("FLARE_VAULT_KEYRING", v),
                    None => std::env::remove_var("FLARE_VAULT_KEYRING"),
                }
            }
        }
    }
    let _guard = KEYRING_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var("FLARE_VAULT_KEYRING").ok();
    unsafe {
        // SAFETY: lock held (above).
        std::env::remove_var("FLARE_VAULT_KEYRING");
    }
    let _reset = ResetOnDrop(prior);
    f()
}

/// Runs `f` with the keyring forced unavailable. Test-only seam.
#[cfg(test)]
pub fn with_keyring_disabled<R>(f: impl FnOnce() -> R) -> R {
    struct ResetOnDrop(Option<String>);
    impl Drop for ResetOnDrop {
        fn drop(&mut self) {
            // SAFETY: KEYRING_ENV_LOCK is held for the whole closure (see
            // below); no other thread can read or write this var concurrently.
            unsafe {
                match self.0.take() {
                    Some(v) => std::env::set_var("FLARE_VAULT_KEYRING", v),
                    None => std::env::remove_var("FLARE_VAULT_KEYRING"),
                }
            }
        }
    }
    let _guard = KEYRING_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var("FLARE_VAULT_KEYRING").ok();
    unsafe {
        // SAFETY: lock held (above).
        std::env::set_var("FLARE_VAULT_KEYRING", "off");
    }
    let _reset = ResetOnDrop(prior);
    f()
}

fn build_service(app_name: &str) -> String {
    format!("{app_name}-vault")
}

pub fn load_from_keyring(app_name: &str, entry_key: &str) -> Option<[u8; 32]> {
    if !keyring_enabled() {
        return None;
    }
    let service = build_service(app_name);
    let entry = keyring::Entry::new(&service, entry_key).ok()?;
    let secret = entry.get_password().ok()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(secret.as_bytes())
        .ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

/// Stores `dek` in the OS keyring, returning whether it is actually
/// retrievable afterwards. A silent backend failure must read as unavailable
/// (so the caller fails closed) rather than as a successful store, hence the
/// read-back verify — cheap, once per unseal.
pub fn store_in_keyring(app_name: &str, entry_key: &str, dek: &[u8; 32]) -> bool {
    if !keyring_enabled() {
        return false;
    }
    let service = build_service(app_name);
    let Ok(entry) = keyring::Entry::new(&service, entry_key) else {
        return false;
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(dek);
    if entry.set_password(&encoded).is_err() {
        return false;
    }
    load_from_keyring(app_name, entry_key).is_some_and(|loaded| loaded == *dek)
}

pub fn clear_keyring(app_name: &str, entry_key: &str) {
    let service = build_service(app_name);
    if let Ok(entry) = keyring::Entry::new(&service, entry_key) {
        let _ = entry.delete_credential();
    }
}

/// Test-only shared in-memory keyring backend. The `keyring` crate's own mock
/// keeps storage per entry instance, so a store/load round-trip across two
/// `Entry::new` calls (exactly what production does) never succeeds against
/// it — and the real OS backends hang without an interactive session, which
/// would wedge the suite. This one shares a process-global map keyed by
/// (service, user), so production code paths run unmodified and
/// deterministically. Entries are unique per test (temp vault paths), so
/// parallel tests never collide.
#[cfg(test)]
pub mod test_support {
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    type MockStore = HashMap<(String, String), Vec<u8>>;

    static STORE: OnceLock<Mutex<MockStore>> = OnceLock::new();

    fn store() -> &'static Mutex<MockStore> {
        STORE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    #[derive(Debug)]
    struct SharedEntry {
        service: String,
        user: String,
    }

    impl keyring::credential::CredentialApi for SharedEntry {
        fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
            store()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((self.service.clone(), self.user.clone()), secret.to_vec());
            Ok(())
        }

        fn get_secret(&self) -> keyring::Result<Vec<u8>> {
            store()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&(self.service.clone(), self.user.clone()))
                .cloned()
                .ok_or(keyring::Error::NoEntry)
        }

        fn delete_credential(&self) -> keyring::Result<()> {
            let removed = store()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&(self.service.clone(), self.user.clone()));
            if removed.is_some() {
                Ok(())
            } else {
                Err(keyring::Error::NoEntry)
            }
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[derive(Debug)]
    struct SharedBuilder;

    impl keyring::credential::CredentialBuilderApi for SharedBuilder {
        fn build(
            &self,
            _target: Option<&str>,
            service: &str,
            user: &str,
        ) -> keyring::Result<Box<keyring::credential::Credential>> {
            Ok(Box::new(SharedEntry {
                service: service.to_string(),
                user: user.to_string(),
            }))
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// Installs the shared mock as this process's keyring backend (idempotent;
    /// call first in every test that needs the "working keyring" path).
    pub fn use_shared_mock_keyring() {
        keyring::set_default_credential_builder(Box::new(SharedBuilder));
    }
}
