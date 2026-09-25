//! Credential resolution for the GitHub client. Order: env → agentflare vault
//! secret (`github_token`) → `gh auth token`. A missing credential is a hard,
//! actionable error — GitHub allows no anonymous writes.

use crate::github::GitHubError;

pub(crate) const NO_AUTH_MSG: &str = "No GitHub credentials. Set GITHUB_TOKEN, run \
'gh auth login', or store one with 'agentflare vault set github_token'.";

fn nonempty(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn env_token() -> Option<String> {
    std::env::var("GITHUB_TOKEN")
        .ok()
        .and_then(nonempty)
        .or_else(|| std::env::var("GH_TOKEN").ok().and_then(nonempty))
}

fn secret_token() -> Option<String> {
    crate::vault::get_secret("github_token")
        .ok()
        .flatten()
        .map(|s| s.to_string())
        .and_then(nonempty)
}

fn gh_auth_token() -> Option<String> {
    let out = flare_process::command("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    nonempty(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The vault/`gh` half of the resolution, cached for the life of the
/// process: `Client::new()` runs on nearly every GitHub-touching path, and
/// without this each construction spawned a `gh auth token` subprocess (and
/// opened the vault). The env half is re-read every time -- it is free, and
/// keeps an explicit `GITHUB_TOKEN` override authoritative.
static CACHED_FALLBACK_TOKEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Resolve a GitHub credential, or `GitHubError::NoAuth` with remediation text.
pub fn resolve_token() -> Result<String, GitHubError> {
    resolve_cached(&CACHED_FALLBACK_TOKEN, env_token(), || {
        pick_token(None, secret_token(), gh_auth_token())
    })
}

/// [`resolve_token`]'s caching, with the cache and the expensive fallback
/// injected so it is testable without touching the process-wide cache that
/// concurrently running tests share.
fn resolve_cached(
    cache: &std::sync::Mutex<Option<String>>,
    env: Option<String>,
    fallback: impl FnOnce() -> Result<String, GitHubError>,
) -> Result<String, GitHubError> {
    if let Some(tok) = env {
        return Ok(tok);
    }
    let mut cached = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(tok) = cached.as_ref() {
        return Ok(tok.clone());
    }
    // Only a successful resolution is cached: a machine with no credential
    // yet must pick one up as soon as the user runs `gh auth login`.
    let tok = fallback()?;
    *cached = Some(tok.clone());
    Ok(tok)
}

/// Drops the cached credential so the next [`resolve_token`] re-resolves it --
/// called by the client when GitHub answers 401, i.e. the cached token was
/// revoked or rotated underneath this process.
pub(crate) fn invalidate_cached_token() {
    *CACHED_FALLBACK_TOKEN
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

fn pick_token(
    env: Option<String>,
    secret: Option<String>,
    gh: Option<String>,
) -> Result<String, GitHubError> {
    env.or(secret)
        .or(gh)
        .ok_or_else(|| GitHubError::NoAuth(NO_AUTH_MSG.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_prefers_env_then_secret_then_gh() {
        assert_eq!(
            pick_token(Some("e".into()), Some("s".into()), Some("g".into())).unwrap(),
            "e"
        );
        assert_eq!(
            pick_token(None, Some("s".into()), Some("g".into())).unwrap(),
            "s"
        );
        assert_eq!(pick_token(None, None, Some("g".into())).unwrap(), "g");
    }

    #[test]
    fn resolve_cached_runs_the_fallback_once_then_serves_the_cache() {
        let cache = std::sync::Mutex::new(None);
        let calls = std::cell::Cell::new(0);
        let fallback = || {
            calls.set(calls.get() + 1);
            Ok("gh-token".to_string())
        };
        assert_eq!(resolve_cached(&cache, None, fallback).unwrap(), "gh-token");
        assert_eq!(resolve_cached(&cache, None, fallback).unwrap(), "gh-token");
        assert_eq!(
            calls.get(),
            1,
            "the gh subprocess must run once per process"
        );
    }

    #[test]
    fn resolve_cached_prefers_env_and_does_not_cache_failures() {
        let cache = std::sync::Mutex::new(None);
        assert_eq!(
            resolve_cached(&cache, Some("env".into()), || unreachable!()).unwrap(),
            "env"
        );
        assert!(resolve_cached(&cache, None, || Err(GitHubError::NoAuth("none".into()))).is_err());
        assert!(cache.lock().unwrap().is_none());
        assert_eq!(
            resolve_cached(&cache, None, || Ok("fresh".into())).unwrap(),
            "fresh"
        );
    }

    #[test]
    fn pick_none_is_noauth() {
        assert!(matches!(
            pick_token(None, None, None).unwrap_err(),
            GitHubError::NoAuth(_)
        ));
    }
}
