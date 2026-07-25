#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_files_return_none_layers() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let layers = locate_and_parse(repo.path(), Some(home.path())).unwrap();
        assert!(layers.project_local.is_none());
        assert!(layers.user_home.is_none());
    }

    #[test]
    fn parses_project_local_file() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".agentflare")).unwrap();
        std::fs::write(
            repo.path().join(".agentflare").join("config.toml"),
            "[git_shim]\nextra_trust_root_paths = [\"x\"]\n",
        )
        .unwrap();
        let layers = locate_and_parse(repo.path(), None).unwrap();
        let (path, value) = layers.project_local.expect("expected project_local layer");
        assert_eq!(path, repo.path().join(".agentflare").join("config.toml"));
        assert_eq!(
            value
                .get("git_shim")
                .and_then(|g| g.get("extra_trust_root_paths")),
            Some(&toml::Value::Array(vec![toml::Value::String("x".into())]))
        );
    }

    #[test]
    fn parses_user_home_file() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".agentflare")).unwrap();
        std::fs::write(
            home.path().join(".agentflare").join("config.toml"),
            "[git_shim]\nextra_trust_root_paths = [\"y\"]\n",
        )
        .unwrap();
        let layers = locate_and_parse(repo.path(), Some(home.path())).unwrap();
        assert!(layers.user_home.is_some());
        assert!(layers.project_local.is_none());
    }

    #[test]
    fn malformed_toml_returns_error_naming_the_file() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".agentflare")).unwrap();
        let bad_path = repo.path().join(".agentflare").join("config.toml");
        std::fs::write(&bad_path, "this is not valid toml [[[").unwrap();
        let err = locate_and_parse(repo.path(), None).unwrap_err();
        assert_eq!(err.path, bad_path);
    }
}
