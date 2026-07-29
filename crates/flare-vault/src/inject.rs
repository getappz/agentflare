use crate::error::VaultResult;
use crate::vault::manager::{
    get_secret_value, list_secret_names, open_vault_with_dek, read_vault_body, VaultPaths,
};
use std::collections::HashMap;
use std::path::Path;

pub fn load_vault_env(app_name: &str, working_dir: &Path) -> VaultResult<HashMap<String, String>> {
    let paths = VaultPaths::project(app_name, working_dir, |p| {
        p.join(".agentflare").exists() || p.join(".git").exists() || p.join("Cargo.toml").exists()
    });

    let mut env = HashMap::new();

    let global_path = paths.global_vault_path();
    if let Ok(dek) = open_vault_with_dek(&global_path, app_name) {
        if let Ok(body) = read_vault_body(&global_path) {
            let names = list_secret_names(&body);
            for name in names {
                if let Ok(Some(val)) = get_secret_value(&body, &dek.dek, &name) {
                    env.insert(name, val.to_string());
                }
            }
        }
    }

    if let Some(project_path) = paths.project_vault_path() {
        if let Ok(dek) = open_vault_with_dek(&project_path, app_name) {
            if let Ok(body) = read_vault_body(&project_path) {
                let names = list_secret_names(&body);
                for name in names {
                    if let Ok(Some(val)) = get_secret_value(&body, &dek.dek, &name) {
                        env.insert(name, val.to_string());
                    }
                }
            }
        }
    }

    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;
    use crate::vault::manager::{create_vault, open_vault, set_secret_value, write_vault_body};
    use tempfile::TempDir;

    #[test]
    fn load_from_global_vault() {
        with_temp_home(|| {
            let app = "flare-vault-test";
            let vault_path = VaultPaths::global(app).global_vault_path();

            create_vault(&vault_path, "pw").unwrap();
            let dek = open_vault(&vault_path, "pw").unwrap();
            let mut body = read_vault_body(&vault_path).unwrap();
            set_secret_value(&mut body, &dek.dek, "MY_SECRET", "test-value").unwrap();
            write_vault_body(&vault_path, &body).unwrap();
            crate::session::store_session(app, &vault_path, &dek.dek);

            // A working dir with no .git/.agentflare/Cargo.toml marker, so
            // this only exercises the global vault, not project scoping.
            let cwd = TempDir::new().unwrap();
            let env = load_vault_env(app, cwd.path()).unwrap();
            assert_eq!(env.get("MY_SECRET").map(String::as_str), Some("test-value"));
        });
    }
}
