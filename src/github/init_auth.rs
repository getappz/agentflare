//! `init`-time guarantee that a GitHub credential exists for a github repo.

#[derive(Debug, PartialEq, Eq)]
pub enum CredState {
    Present(&'static str),
    Missing,
}

pub fn classify(env_present: bool, secret_present: bool, gh_present: bool) -> CredState {
    if env_present {
        CredState::Present("GITHUB_TOKEN")
    } else if secret_present {
        CredState::Present("stored secret")
    } else if gh_present {
        CredState::Present("gh")
    } else {
        CredState::Missing
    }
}

fn env_present() -> bool {
    ["GITHUB_TOKEN", "GH_TOKEN"].iter().any(|v| {
        std::env::var(v)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    })
}

fn gh_present() -> bool {
    std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

fn secret_present() -> bool {
    crate::db::open()
        .ok()
        .and_then(|conn| {
            crate::gateway_secrets::get_secret(&conn, "github_token")
                .ok()
                .flatten()
        })
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

pub fn is_github_repo(repo_root: &std::path::Path) -> bool {
    crate::git::run_in_opt(repo_root, &["remote", "get-url", "origin"])
        .map(|u| u.contains("github"))
        .unwrap_or(false)
}

fn store_token(token: &str) -> Result<(), String> {
    let conn = crate::db::open().map_err(|e| e.to_string())?;
    crate::gateway_secrets::set_secret(&conn, "github_token", token).map_err(|e| e.to_string())
}

/// Ensure a GitHub credential exists for a github repo. Never blocks under
/// `-y`/non-interactive.
pub fn ensure(agent: &str, yes: bool) {
    let cwd = std::env::current_dir().unwrap_or_default();
    if !is_github_repo(&cwd) {
        return;
    }
    match classify(env_present(), secret_present(), gh_present()) {
        CredState::Present(src) => {
            crate::ui::skip(&format!("GitHub credential present (via {src})"));
        }
        CredState::Missing => {
            crate::ui::info(
                "No GitHub credential found — flare_git writes (PRs, issues, releases) need one.",
            );
            if yes || !crate::ui::interactive() {
                crate::ui::skip(
                    "non-interactive: run gh auth login or set GITHUB_TOKEN to enable flare_git writes",
                );
                return;
            }
            if !crate::init::prompt_yes(
                "Store a GitHub token now? (or run 'gh auth login' later)",
                agent,
                yes,
            ) {
                return;
            }
            let Some(token) = crate::ui::password("GitHub PAT") else {
                crate::ui::skip("no token entered");
                return;
            };
            match store_token(&token) {
                Ok(()) => crate::ui::success("stored github_token secret (encrypted)"),
                Err(e) => crate::ui::error(&format!("storing github_token: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classify_prefers_env_then_secret_then_gh_then_missing() {
        assert_eq!(
            classify(true, true, true),
            CredState::Present("GITHUB_TOKEN")
        );
        assert_eq!(
            classify(false, true, true),
            CredState::Present("stored secret")
        );
        assert_eq!(classify(false, false, true), CredState::Present("gh"));
        assert_eq!(classify(false, false, false), CredState::Missing);
    }
}
