//! Repo identity — parse `owner/repo` from any git remote form (HTTPS, scp-like
//! SSH, ssh:// URLs, SSH host aliases, trailing `.git`), and resolve it from a
//! working tree's `origin` remote.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoId {
    pub owner: String,
    pub repo: String,
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.repo)
    }
}

impl RepoId {
    pub fn parse(remote_url: &str) -> Option<RepoId> {
        let norm = normalize_repo(remote_url);
        let (owner, repo) = norm.split_once('/')?;
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        Some(RepoId {
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    pub fn resolve_from_remote(repo_root: &Path) -> Option<RepoId> {
        let url = crate::git::run_in_opt(repo_root, &["remote", "get-url", "origin"])?;
        RepoId::parse(&url)
    }
}

/// Normalize any git remote URL to `owner/repo`. Handles HTTPS, scp-like SSH
/// (`host:owner/repo`), `ssh://`, SSH host aliases, ports, and trailing `.git`.
pub fn normalize_repo(remote_url: &str) -> String {
    let s = remote_url.trim().trim_end_matches('/');
    let after_scheme = s.split("://").last().unwrap_or(s);
    let path = match after_scheme.split_once(':') {
        Some((_host, path)) if !path.starts_with('/') => path,
        _ => after_scheme
            .split_once('/')
            .map(|x| x.1)
            .unwrap_or(after_scheme),
    };
    let path = path.trim_start_matches('/').trim_end_matches(".git");
    let segs: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    match segs.as_slice() {
        [.., owner, name] => format!("{owner}/{name}"),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_parses_all_remote_forms() {
        for url in [
            "https://github.com/getappz/agentflare.git",
            "https://github.com/getappz/agentflare",
            "git@github.com:getappz/agentflare.git",
            "git@github-appzdev:getappz/agentflare.git",
            "ssh://git@github.com/getappz/agentflare.git",
        ] {
            let id = RepoId::parse(url).unwrap();
            assert_eq!(id.owner, "getappz");
            assert_eq!(id.repo, "agentflare");
            assert_eq!(id.to_string(), "getappz/agentflare");
        }
    }

    #[test]
    fn repo_id_parse_is_none_for_single_segment() {
        assert!(RepoId::parse("not-a-url").is_none());
    }
}
