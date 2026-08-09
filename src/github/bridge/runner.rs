//! Daemon-side loop. `ureq` is blocking, so this is a dedicated std thread
//! rather than a tokio task — never call the GitHub client from the async
//! runtime without `spawn_blocking`.

use crate::github::bridge::config::BridgeConfig;
use crate::github::bridge::tick::{Ctx, run_once};

pub fn should_run(config: &BridgeConfig) -> bool {
    config.enabled
}

/// `owner/repo` from `AGENTFLARE_BRIDGE_REPO`, else the `origin` remote of
/// the current directory.
///
/// The env var is not a convenience: `resolve_from_remote` derives the repo
/// from **cwd**, and neither the launchd plist nor the systemd unit
/// (`daemon_autostart.rs`) sets a working directory. Under
/// `agentflare daemon start` the bridge would otherwise be enabled by env
/// var and then silently never run, because cwd is not the repo.
fn resolve_repo() -> Option<crate::github::RepoId> {
    if let Some(explicit) = std::env::var("AGENTFLARE_BRIDGE_REPO")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        let parsed = crate::github::RepoId::parse(explicit.trim());
        if parsed.is_none() {
            eprintln!(
                "github bridge: AGENTFLARE_BRIDGE_REPO={explicit:?} is not a \
                 GitHub owner/repo; not starting"
            );
        }
        return parsed;
    }

    let repo_root = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("github bridge: cannot read the working directory ({e}); not starting");
            return None;
        }
    };
    let repo = crate::github::RepoId::resolve_from_remote(&repo_root);
    if repo.is_none() {
        eprintln!(
            "github bridge: no GitHub `origin` remote under {} — the daemon \
             does not set a working directory, so set AGENTFLARE_BRIDGE_REPO=owner/repo; \
             not starting",
            repo_root.display()
        );
    }
    repo
}

/// Starts the poll loop if the bridge is enabled AND the environment is
/// usable (GitHub origin, resolvable credential, resolvable project).
///
/// Every failure path is a no-op — a daemon must not fail to start because an
/// optional subsystem is unconfigured — but never a SILENT one. The bridge is
/// opt-in, so anyone reaching these paths asked for it to run and needs to be
/// told why it did not.
/// Nothing here touches the network or the filesystem, so the caller — the
/// daemon, on its way to binding the dashboard port — returns immediately.
/// Everything that can block (shelling out to `git remote`, and to
/// `gh auth token` for a credential) happens on the spawned thread.
pub fn spawn_if_enabled() -> Option<std::thread::JoinHandle<()>> {
    let config = BridgeConfig::from_env();
    if !should_run(&config) {
        return None;
    }
    Some(std::thread::spawn(move || run_forever(config)))
}

/// The bridge's own item-database connection.
///
/// Deliberately NOT `AgentflareMcp::with_backend_db`: that holds a mutex for
/// the duration of the closure, and a whole tick's worth of GitHub round
/// trips runs inside it — so every MCP tool call and dashboard read would
/// block behind the bridge's network latency. A second SQLite connection to
/// the same file is the normal way to give a background thread its own
/// handle.
fn open_items_db() -> Option<rusqlite::Connection> {
    let path = crate::paths::home().join(".agentflare").join("backend.db");
    match agentflare_backend::db::open_db(&path) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!(
                "github bridge: item database unavailable at {} ({e}); not starting",
                path.display()
            );
            None
        }
    }
}

fn build_ctx(config: BridgeConfig, conn: &rusqlite::Connection) -> Option<Ctx> {
    let repo = resolve_repo()?;
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("github bridge: no usable credential ({e}); not starting");
            return None;
        }
    };
    let mcp = crate::mcp_server::AgentflareMcp::default();
    let project_id = match mcp.resolve_project(conn) {
        Ok(p) => p.id,
        Err(e) => {
            eprintln!("github bridge: no project linked to this repo ({e}); not starting");
            return None;
        }
    };
    // The claim ledger is its own database (agentflare.db), separate from the
    // backend db that holds items.
    let ledger = match crate::db::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("github bridge: claim ledger unavailable ({e}); not starting");
            return None;
        }
    };
    Some(Ctx {
        client,
        repo,
        config,
        project_id,
        ledger,
    })
}

fn run_forever(config: BridgeConfig) {
    let interval = std::time::Duration::from_secs(config.interval_secs);
    let Some(conn) = open_items_db() else { return };
    let Some(ctx) = build_ctx(config, &conn) else {
        return;
    };

    // Soft errors repeat every tick by nature, so log a CHANGE rather than
    // each occurrence: 1440 identical lines a day would bury the one thing
    // worth reading, and silence would hide a `Forbidden` — a bridge that is
    // permanently dead and never says so.
    let mut last_soft: Option<String> = None;
    loop {
        let now = crate::claims::now();
        match run_once(&ctx, &conn, now) {
            Ok(report) => {
                if !report.claimed.is_empty() || !report.ceded.is_empty() {
                    eprintln!(
                        "github bridge: claimed {:?} ceded {:?}",
                        report.claimed, report.ceded
                    );
                }
                if report.soft_error != last_soft {
                    match &report.soft_error {
                        Some(e) => eprintln!("github bridge: ticks are ending early: {e}"),
                        None => eprintln!("github bridge: recovered; ticks completing again"),
                    }
                    last_soft = report.soft_error;
                }
            }
            Err(e) => eprintln!("github bridge: tick failed: {e}"),
        }
        std::thread::sleep(interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_spawns_nothing() {
        let cfg = crate::github::bridge::config::BridgeConfig::from_values(
            Some("0"),
            None,
            None,
            None,
            None,
            "a:1".to_string(),
        );
        assert!(!should_run(&cfg));
    }

    #[test]
    fn enabled_config_runs() {
        let cfg = crate::github::bridge::config::BridgeConfig::from_values(
            Some("1"),
            None,
            None,
            None,
            None,
            "a:1".to_string(),
        );
        assert!(should_run(&cfg));
    }
}
