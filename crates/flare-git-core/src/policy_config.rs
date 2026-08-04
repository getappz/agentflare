use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::classify::{
    ALLOWED_MUTATING_SUBCOMMANDS, DENIED_PLUMBING_SUBCOMMANDS, TRUST_ROOT_PATHS,
    extra_trust_root_paths_from_env,
};
use crate::config_loader::{self, LoaderError};

#[derive(Debug, Default, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    git_shim: GitShimConfig,
}

#[derive(Debug, Default, Deserialize)]
struct GitShimConfig {
    #[serde(default)]
    extra_trust_root_paths: Vec<String>,
    #[serde(default)]
    extra_allowed_mutating_subcommands: Vec<String>,
    #[serde(default)]
    extra_denied_plumbing_subcommands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedGitShimPolicy {
    pub trust_root_paths: Vec<String>,
    pub allowed_mutating_subcommands: Vec<String>,
    pub denied_plumbing_subcommands: Vec<String>,
}

impl ResolvedGitShimPolicy {
    #[must_use]
    pub fn baseline() -> Self {
        Self {
            trust_root_paths: unioned(TRUST_ROOT_PATHS, [&extra_trust_root_paths_from_env()]),
            allowed_mutating_subcommands: ALLOWED_MUTATING_SUBCOMMANDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            denied_plumbing_subcommands: DENIED_PLUMBING_SUBCOMMANDS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

fn unioned<const N: usize>(baseline: &[&str], extra_layers: [&Vec<String>; N]) -> Vec<String> {
    let mut out: Vec<String> = baseline.iter().map(|s| (*s).to_string()).collect();
    for layer in extra_layers {
        for item in layer {
            if !out.contains(item) {
                out.push(item.clone());
            }
        }
    }
    out
}

fn parse_git_shim(layer: Option<(PathBuf, toml::Value)>) -> Result<GitShimConfig, LoaderError> {
    let Some((path, value)) = layer else {
        return Ok(GitShimConfig::default());
    };
    ConfigFile::deserialize(value)
        .map(|f| f.git_shim)
        .map_err(|source| LoaderError {
            path,
            source: Box::new(source),
        })
}

pub fn resolve(
    repo_root: &Path,
    home: Option<&Path>,
) -> Result<ResolvedGitShimPolicy, LoaderError> {
    let layers = config_loader::locate_and_parse(repo_root, home)?;
    let project_local = parse_git_shim(layers.project_local)?;
    let user_home = parse_git_shim(layers.user_home)?;

    Ok(ResolvedGitShimPolicy {
        trust_root_paths: unioned(
            TRUST_ROOT_PATHS,
            [
                &project_local.extra_trust_root_paths,
                &user_home.extra_trust_root_paths,
                &extra_trust_root_paths_from_env(),
            ],
        ),
        allowed_mutating_subcommands: unioned(
            ALLOWED_MUTATING_SUBCOMMANDS,
            [
                &project_local.extra_allowed_mutating_subcommands,
                &user_home.extra_allowed_mutating_subcommands,
            ],
        ),
        denied_plumbing_subcommands: unioned(
            DENIED_PLUMBING_SUBCOMMANDS,
            [
                &project_local.extra_denied_plumbing_subcommands,
                &user_home.extra_denied_plumbing_subcommands,
            ],
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_files_no_env_resolves_to_baseline() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let resolved = resolve(repo.path(), Some(home.path())).unwrap();
        assert_eq!(resolved, ResolvedGitShimPolicy::baseline());
    }

    #[test]
    fn project_local_and_user_home_union_and_dedup() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".agentflare")).unwrap();
        std::fs::write(
            repo.path().join(".agentflare").join("config.toml"),
            "[git_shim]\nextra_trust_root_paths = [\"proj/\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(home.path().join(".agentflare")).unwrap();
        std::fs::write(
            home.path().join(".agentflare").join("config.toml"),
            "[git_shim]\nextra_trust_root_paths = [\"proj/\", \"home/\"]\n",
        )
        .unwrap();

        let resolved = resolve(repo.path(), Some(home.path())).unwrap();
        let mut expected = ResolvedGitShimPolicy::baseline().trust_root_paths;
        expected.push("proj/".to_string());
        expected.push("home/".to_string());
        assert_eq!(resolved.trust_root_paths, expected);
    }

    #[test]
    fn malformed_config_returns_error() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".agentflare")).unwrap();
        std::fs::write(
            repo.path().join(".agentflare").join("config.toml"),
            "not valid toml [[[",
        )
        .unwrap();
        assert!(resolve(repo.path(), None).is_err());
    }
}
