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
                    env.insert(name, val);
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
                        env.insert(name, val);
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
    use crate::vault::manager::{create_vault, open_vault, set_secret_value, write_vault_body};
    use tempfile::TempDir;

    #[test]
    fn load_from_global_vault() {
        let app = "flare-vault-test";
        let dir = TempDir::new().unwrap();
        let vault_path = dir.path().join(".flare-vault-test").join("vault.json");

        create_vault(&vault_path, "pw").unwrap();
        let dek = open_vault(&vault_path, "pw").unwrap();
        let mut body = read_vault_body(&vault_path).unwrap();
        set_secret_value(&mut body, &dek.dek, "MY_SECRET", "test-value").unwrap();
        write_vault_body(&vault_path, &body).unwrap();

        let result = load_vault_env(app, dir.path());
        assert!(result.is_ok());
    }
}
