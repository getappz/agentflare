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
        // An already-resolved `owner/repo` identifier (e.g. an explicit
        // `--repo` flag) has no host to validate — accept it directly.
        if let Some((owner, repo)) = bare_owner_repo(remote_url) {
            return Some(RepoId { owner, repo });
        }
        // Issue #224: reject non-GitHub origins. A GitLab/Bitbucket
        // remote would otherwise resolve to a same-named GitHub repo and
        // write ops would target GitHub with a GitHub token.
        confirmed_github_host(remote_url)?;
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

/// Recognizes an already-resolved `owner/repo` identifier — no scheme, no
/// `@`/`:` (which would indicate a URL or scp-like SSH remote), exactly two
/// non-empty segments. Callers pass this form explicitly (e.g. `--repo`),
/// so it has no host to validate against the issue #224 gate.
fn bare_owner_repo(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    if s.contains("://") || s.contains('@') || s.contains(':') {
        return None;
    }
    let (owner, repo) = s.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// Allowed GitHub host suffixes. A remote whose resolved host equals one
/// of these (or ends with `.{host}`) is treated as a GitHub origin.
/// Override via AGENTFLARE_GITHUB_HOSTS (comma-separated); defaults to github.com.
fn allowed_github_hosts() -> Vec<String> {
    std::env::var("AGENTFLARE_GITHUB_HOSTS")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.split(',')
                .map(|h| h.trim().to_lowercase())
                .filter(|h| !h.is_empty())
                .collect()
        })
        .unwrap_or_else(|| vec!["github.com".to_string()])
}

/// Extract the host from any git remote URL form (mirrors the host
/// handling implied by `normalize_repo`): scp-like `git@host:path`,
/// `ssh://git@host:port/path`, and `https://host/path`.
fn repo_host(remote_url: &str) -> Option<String> {
    let s = remote_url.trim().trim_end_matches('/');
    let after_scheme = s.split("://").last().unwrap_or(s);
    let (host, _) = match after_scheme.split_once(':') {
        Some((host, path)) if !path.starts_with('/') => (host, path),
        _ => {
            let auth_stripped = after_scheme
                .split_once('@')
                .map(|(_, r)| r)
                .unwrap_or(after_scheme);
            let host_part = auth_stripped.split('/').next().unwrap_or(auth_stripped);
            (
                host_part.split(':').next().unwrap_or(host_part),
                after_scheme,
            )
        }
    };
    let host = host.trim_start_matches("git@");
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Resolve an SSH host alias to its real hostname via `ssh -G` (ground-truth
/// ssh config resolution; works offline, handles HostName/Match/Include).
/// Returns None if ssh is unavailable or emits no `hostname` line.
fn resolve_ssh_alias(host: &str) -> Option<String> {
    let out = std::process::Command::new("ssh")
        .arg("-G")
        .arg(host)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("hostname ") {
            let v = val.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// True for SSH-transported remotes: scp-like `[user@]host:path` or an
/// explicit `ssh://` scheme. False for `https://`/`http://` — those hosts
/// are literal DNS names, never SSH config aliases, so they don't need (and
/// must not trigger) `ssh -G` resolution.
fn is_ssh_remote(remote_url: &str) -> bool {
    let s = remote_url.trim();
    if s.starts_with("ssh://") {
        return true;
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return false;
    }
    match (s.find(':'), s.find('/')) {
        (Some(colon), Some(slash)) => colon < slash,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Resolve + validate: returns the real (alias-resolved) host iff it is an
/// allowed GitHub host, else None. This is the gate for issue #224.
fn confirmed_github_host(remote_url: &str) -> Option<String> {
    let host = repo_host(remote_url)?;
    let hosts = allowed_github_hosts();
    let matches = |h: &str| {
        hosts
            .iter()
            .any(|allowed| h == allowed || h.ends_with(&format!(".{allowed}")))
    };

    // Fast path: already a direct GitHub host, no alias resolution needed —
    // skips spawning `ssh` on the common case.
    let host_lc = host.to_lowercase();
    if matches(&host_lc) {
        return Some(host);
    }
    // Only SSH-shaped remotes can be host aliases needing `ssh -G`
    // resolution — an HTTPS (or otherwise unrecognized) host is literal, so
    // no match here means "not GitHub": reject without spawning ssh.
    if !is_ssh_remote(remote_url) {
        return None;
    }
    // A host starting with `-` would be parsed by `ssh` as an option rather
    // than a hostname (argument injection) — never pass it through.
    if host.starts_with('-') {
        return None;
    }
    let resolved = resolve_ssh_alias(&host)?;
    matches(&resolved.to_lowercase()).then_some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_id_parses_all_remote_forms() {
        // The bare `github-appzdev` SSH alias is environment-dependent (only
        // resolvable where `ssh -G` reaches a ~/.ssh/config with that HostName
        // and `ssh` is functional) — it is covered by the guarded test below,
        // not this always-runs loop.
        for url in [
            "https://github.com/getappz/agentflare.git",
            "https://github.com/getappz/agentflare",
            "git@github.com:getappz/agentflare.git",
            "ssh://git@github.com/getappz/agentflare.git",
        ] {
            let id = RepoId::parse(url).unwrap();
            assert_eq!(id.owner, "getappz");
            assert_eq!(id.repo, "agentflare");
            assert_eq!(id.to_string(), "getappz/agentflare");
        }
    }

    #[test]
    fn repo_id_accepts_ssh_alias_only_when_resolvable_to_github() {
        // Issue #224: an SSH host alias is only accepted as a GitHub origin if
        // `ssh -G` resolves it to a github.com hostname. Where that alias is
        // absent (CI without the alias, or a sandbox without a working `ssh`),
        // skip rather than fail.
        if !ssh_alias_resolves_to_github("github-appzdev") {
            eprintln!(
                "skipping alias test: `ssh -G github-appzdev` does not resolve to github.com"
            );
            return;
        }
        let id = RepoId::parse("git@github-appzdev:getappz/agentflare.git").unwrap();
        assert_eq!(id.owner, "getappz");
        assert_eq!(id.repo, "agentflare");
        assert_eq!(id.to_string(), "getappz/agentflare");
    }

    /// Returns true iff `ssh -G <host>` succeeds and emits `hostname github.com`.
    /// Used to guard the environment-dependent alias-acceptance test.
    fn ssh_alias_resolves_to_github(host: &str) -> bool {
        match std::process::Command::new("ssh")
            .arg("-G")
            .arg(host)
            .output()
        {
            Ok(o) if o.status.success() => {
                let text = String::from_utf8_lossy(&o.stdout);
                text.lines().any(|l| {
                    let l = l.trim();
                    l.strip_prefix("hostname ")
                        .map(|v| v.trim().eq_ignore_ascii_case("github.com"))
                        .unwrap_or(false)
                })
            }
            _ => false,
        }
    }

    #[test]
    fn repo_id_parse_is_none_for_single_segment() {
        assert!(RepoId::parse("not-a-url").is_none());
    }

    #[test]
    fn normalize_repo_handles_ports_and_extra_path_segments() {
        // ssh:// with an explicit port, and a deeper path than owner/repo.
        assert_eq!(
            normalize_repo("ssh://git@github.com:22/getappz/agentflare.git"),
            "getappz/agentflare"
        );
        assert_eq!(
            normalize_repo("https://gitlab.com/group/subgroup/proj.git"),
            "subgroup/proj"
        );
    }

    #[test]
    fn normalize_repo_trims_trailing_slash_and_git_suffix() {
        assert_eq!(normalize_repo("https://github.com/o/r.git/"), "o/r");
    }

    #[test]
    fn normalize_repo_passes_through_a_bare_single_segment() {
        assert_eq!(normalize_repo("justname"), "justname");
    }

    #[test]
    fn repo_id_parse_is_none_for_empty_input() {
        assert!(RepoId::parse("").is_none());
    }

    #[test]
    fn repo_host_extracts_host_from_all_forms() {
        assert_eq!(
            repo_host("https://github.com/o/r.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            repo_host("git@github.com:o/r.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            repo_host("ssh://git@github.com:22/o/r.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            repo_host("git@github-appzdev:o/r.git").as_deref(),
            Some("github-appzdev")
        );
        assert_eq!(
            repo_host("https://gitlab.com/o/r.git").as_deref(),
            Some("gitlab.com")
        );
    }

    #[test]
    fn repo_id_parse_rejects_non_github_origins() {
        assert!(RepoId::parse("git@gitlab.com:o/r.git").is_none());
        assert!(RepoId::parse("https://bitbucket.org/o/r").is_none());
        assert!(RepoId::parse("ssh://git@gitlab.com/o/r.git").is_none());
    }

    #[test]
    fn confirmed_github_host_rejects_dash_prefixed_host_without_invoking_ssh() {
        // A `-`-prefixed host must never reach `ssh -G <host>` as an
        // argument — it would be parsed as an option (argument injection)
        // rather than a hostname.
        assert!(RepoId::parse("git@-oProxyCommand=x:o/r.git").is_none());
    }

    #[test]
    fn repo_id_parse_accepts_github_origins() {
        let id = RepoId::parse("https://github.com/o/r.git").unwrap();
        assert_eq!((id.owner.as_str(), id.repo.as_str()), ("o", "r"));
        let id = RepoId::parse("git@github.com:o/r.git").unwrap();
        assert_eq!((id.owner.as_str(), id.repo.as_str()), ("o", "r"));
        let id = RepoId::parse("ssh://git@github.com/o/r.git").unwrap();
        assert_eq!((id.owner.as_str(), id.repo.as_str()), ("o", "r"));
    }

    #[test]
    fn repo_id_parse_accepts_bare_owner_repo_without_host_check() {
        // Explicit `--repo owner/name` (flare_git's documented format) has
        // no host to validate — it must bypass the issue #224 gate entirely,
        // even though "owner" alone isn't a github.com host.
        let id = RepoId::parse("getappz/agentflare").unwrap();
        assert_eq!(
            (id.owner.as_str(), id.repo.as_str()),
            ("getappz", "agentflare")
        );
        assert!(RepoId::parse("owner/repo/extra").is_none());
        assert!(RepoId::parse("owner/").is_none());
    }

    #[test]
    fn confirmed_github_host_rejects_non_ssh_host_without_invoking_ssh() {
        // An HTTPS remote's host is a literal DNS name, never an SSH config
        // alias — a non-allowlisted HTTPS host must be rejected without
        // spawning `ssh -G`.
        assert!(RepoId::parse("https://gitlab.com/o/r").is_none());
        assert!(!is_ssh_remote("https://gitlab.com/o/r"));
        assert!(is_ssh_remote("git@gitlab.com:o/r.git"));
    }

    #[test]
    fn allowed_github_hosts_honors_env() {
        // AGENTFLARE_GITHUB_HOSTS mutation is process-global — serialize
        // against every other env mutation in this test binary and restore
        // the original value rather than unconditionally removing it.
        let _guard = agent_registry::detect::PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var_os("AGENTFLARE_GITHUB_HOSTS");
        unsafe {
            // SAFETY: PATH_LOCK serializes all env mutations in this binary.
            std::env::set_var("AGENTFLARE_GITHUB_HOSTS", "github.com,git.example.com");
        }
        let hosts = allowed_github_hosts();
        assert!(hosts.contains(&"github.com".to_string()));
        assert!(hosts.contains(&"git.example.com".to_string()));
        match original {
            Some(v) => unsafe { std::env::set_var("AGENTFLARE_GITHUB_HOSTS", v) },
            None => unsafe { std::env::remove_var("AGENTFLARE_GITHUB_HOSTS") },
        }
    }
}
