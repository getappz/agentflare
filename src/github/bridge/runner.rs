//! Daemon-side loop. `ureq` is blocking, so this is a dedicated std thread
//! rather than a tokio task — never call the GitHub client from the async
//! runtime without `spawn_blocking`.

use crate::github::bridge::config::BridgeConfig;
use crate::github::bridge::tick::{Ctx, run_once};

pub fn should_run(config: &BridgeConfig) -> bool {
    config.enabled
}

/// Starts the poll loop if the bridge is enabled AND the environment is
/// usable (GitHub origin, resolvable credential, resolvable project).
/// Every failure path here is a quiet no-op: a daemon must not fail to start
/// because an optional subsystem is unconfigured.
pub fn spawn_if_enabled() -> Option<std::thread::JoinHandle<()>> {
    let config = BridgeConfig::from_env();
    if !should_run(&config) {
        return None;
    }

    let repo_root = std::env::current_dir().ok()?;
    let repo = crate::github::RepoId::resolve_from_remote(&repo_root)?;
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("github bridge: no usable credential ({e}); not starting");
            return None;
        }
    };

    let mcp = crate::mcp_server::AgentflareMcp::default();
    let project_id = mcp
        .with_backend_db(|conn| mcp.resolve_project(conn).map(|p| p.id))
        .ok()?
        .ok()?;

    // The claim ledger is its own database (agentflare.db), separate from the
    // backend db that holds items.
    let ledger = match crate::db::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("github bridge: claim ledger unavailable ({e}); not starting");
            return None;
        }
    };

    let interval = std::time::Duration::from_secs(config.interval_secs);
    let ctx = Ctx {
        client,
        repo,
        config,
        project_id,
        ledger,
    };

    Some(std::thread::spawn(move || {
        loop {
            let now = crate::claims::now();
            let outcome = mcp.with_backend_db(|conn| run_once(&ctx, conn, now));
            match outcome {
                Ok(Ok(report)) => {
                    if !report.claimed.is_empty() || !report.ceded.is_empty() {
                        eprintln!(
                            "github bridge: claimed {:?} ceded {:?}",
                            report.claimed, report.ceded
                        );
                    }
                }
                Ok(Err(e)) => eprintln!("github bridge: tick failed: {e}"),
                Err(e) => eprintln!("github bridge: backend unavailable: {e}"),
            }
            std::thread::sleep(interval);
        }
    }))
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
            "a:1".to_string(),
        );
        assert!(should_run(&cfg));
    }
}
