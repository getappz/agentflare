use agentflare_store::Store;
use std::path::PathBuf;
use std::sync::OnceLock;

static STORE: OnceLock<Result<Store, agentflare_store::Error>> = OnceLock::new();

pub fn store_path() -> PathBuf {
    crate::paths::home().join(".agentflare").join("agentflare.db")
}

pub fn open() -> &'static Store {
    STORE
        .get_or_init(|| {
            let path = store_path();
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            Store::open_file(&path)
        })
        .as_ref()
        .expect("failed to open agentflare store")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn open_and_close() {
        with_temp_home(|| {
            let store = open();
            store.conn().execute_batch("SELECT 1").unwrap();
        });
    }
}
