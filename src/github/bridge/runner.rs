//! Daemon-side loop. `ureq` is blocking, so this is a dedicated std thread
//! rather than a tokio task — never call the GitHub client from the async
//! runtime without `spawn_blocking`.

use crate::github::RepoId;
use crate::github::bridge::config::BridgeConfig;
use crate::github::bridge::tick::{Ctx, run_once};

pub fn should_run(config: &BridgeConfig) -> bool {
    config.enabled
}

/// One repo this daemon should poll this tick, with the project/label/agent
/// it maps to.
struct BridgeTarget {
    repo: RepoId,
    project_id: String,
    queue_label: String,
    work_agent: Option<String>,
}

/// Repos to poll this tick: every row in `bridge_repos` (the reverse index
/// of `.agentflare/project.json`, refreshed by `resolve_project` wherever an
/// agentflare CLI/MCP call runs inside a linked repo) plus, for back-compat
/// with the single-repo manual/dev workflow, `AGENTFLARE_BRIDGE_REPO` if it
/// names a repo not already covered by the registry.
///
/// Read fresh every tick, not just at startup, so a project linked (or
/// re-linked) after the daemon started shows up without a restart.
fn bridge_targets(conn: &rusqlite::Connection) -> Vec<BridgeTarget> {
    let mut targets: Vec<BridgeTarget> = agentflare_backend::bridge_repo::list(conn)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            let repo = RepoId::parse(&row.repo)?;
            Some(BridgeTarget {
                repo,
                project_id: row.project_id,
                queue_label: row.queue_label,
                work_agent: row.work_agent,
            })
        })
        .collect();

    if let Some(explicit) = std::env::var("AGENTFLARE_BRIDGE_REPO")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        let Some(repo) = RepoId::parse(explicit.trim()) else {
            eprintln!(
                "github bridge: AGENTFLARE_BRIDGE_REPO={explicit:?} is not a \
                 GitHub owner/repo; ignoring"
            );
            return targets;
        };
        if !targets.iter().any(|t| t.repo == repo) {
            // Same resolution `build_ctx` used to do unconditionally: cwd-based,
            // which only makes sense here because this override is for the
            // interactive case (daemon started by hand from inside the repo),
            // not the systemd/launchd path the registry above covers.
            let repo_root = std::env::current_dir().unwrap_or_default();
            let mcp = crate::mcp_server::AgentflareMcp::default();
            match mcp.resolve_project(conn) {
                Ok(p) => targets.push(BridgeTarget {
                    repo,
                    project_id: p.id,
                    queue_label: crate::github::bridge::config::resolve_project_queue_label(
                        &repo_root,
                    ),
                    work_agent: None,
                }),
                Err(e) => eprintln!(
                    "github bridge: AGENTFLARE_BRIDGE_REPO={explicit:?} set but no project \
                     linked ({e}); ignoring"
                ),
            }
        }
    }
    targets
}

/// Starts the poll loop if the bridge is enabled.
///
/// Unlike before this no longer requires a resolvable repo up front — which
/// repos to poll is now read from the registry every tick (see
/// `bridge_targets`), so an empty registry at startup isn't fatal, just
/// nothing to do yet.
/// Nothing here touches the network or the filesystem, so the caller — the
/// daemon, on its way to binding the dashboard port — returns immediately.
/// Everything that can block (credential resolution, GitHub calls) happens
/// on the spawned thread.
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

fn run_forever(config: BridgeConfig) {
    let interval = std::time::Duration::from_secs(config.interval_secs);
    let Some(conn) = open_items_db() else { return };
    let client = match crate::github::Client::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("github bridge: no usable credential ({e}); not starting");
            return;
        }
    };

    // Soft errors repeat every tick by nature, so log a CHANGE per repo
    // rather than each occurrence: 1440 identical lines a day would bury the
    // one thing worth reading, and silence would hide a `Forbidden` — a repo
    // that is permanently unreachable and never says so. Keyed by repo since
    // one daemon now polls several independently.
    let mut last_soft: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut had_targets = true; // forces the "nothing registered" message on the first empty tick
    loop {
        let targets = bridge_targets(&conn);
        if targets.is_empty() {
            if had_targets {
                eprintln!(
                    "github bridge: no repos registered — link a project from inside a repo \
                     (any agentflare CLI/MCP call there) or set AGENTFLARE_BRIDGE_REPO"
                );
                had_targets = false;
            }
            std::thread::sleep(interval);
            continue;
        }
        had_targets = true;

        for target in targets {
            // The claim ledger lives in its own database (agentflare.db),
            // separate from the backend db that holds items. Reopened per
            // repo per tick (a local sqlite open is cheap) rather than
            // shared, so one repo's failure here only skips that repo
            // instead of taking down the whole loop for the rest of the tick.
            let ledger = match crate::db::open() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "github bridge: claim ledger unavailable ({e}); skipping remaining \
                         repos this tick"
                    );
                    break;
                }
            };
            let mut repo_config = config.clone();
            repo_config.queue_label = target.queue_label;
            repo_config.work_agent = target.work_agent.or(repo_config.work_agent);
            let ctx = Ctx {
                client: client.clone(),
                repo: target.repo,
                config: repo_config,
                project_id: target.project_id,
                ledger,
            };
            let now = crate::claims::now();
            let repo_key = ctx.repo.to_string();
            match run_once(&ctx, &conn, now) {
                Ok(report) => {
                    if !report.claimed.is_empty() || !report.ceded.is_empty() {
                        eprintln!(
                            "github bridge: {repo_key} claimed {:?} ceded {:?}",
                            report.claimed, report.ceded
                        );
                    }
                    let prev = last_soft.get(&repo_key).cloned();
                    if report.soft_error != prev {
                        match &report.soft_error {
                            Some(e) => {
                                eprintln!("github bridge: {repo_key} ticks are ending early: {e}");
                                last_soft.insert(repo_key, e.clone());
                            }
                            None => {
                                eprintln!(
                                    "github bridge: {repo_key} recovered; ticks completing again"
                                );
                                last_soft.remove(&repo_key);
                            }
                        }
                    }
                }
                Err(e) => eprintln!("github bridge: {repo_key} tick failed: {e}"),
            }
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

    #[test]
    fn bridge_targets_reads_every_registered_repo() {
        let _guard = agent_registry::detect::PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("AGENTFLARE_BRIDGE_REPO");
        }

        let conn = agentflare_backend::open_in_memory().unwrap();
        let workspace = agentflare_backend::workspace::create(
            &conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "ws".into(),
                slug: "ws".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let mk_project = |name: &str| {
            agentflare_backend::project::create(
                &conn,
                agentflare_backend::project::CreateProject {
                    workspace_id: workspace.id.clone(),
                    name: name.into(),
                    identifier: name.into(),
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap()
        };
        let p1 = mk_project("proj-one");
        let p2 = mk_project("proj-two");
        agentflare_backend::bridge_repo::upsert(
            &conn,
            "getappz/one",
            &p1.id,
            "/repo/one",
            "agentflare",
            None,
            1,
        )
        .unwrap();
        agentflare_backend::bridge_repo::upsert(
            &conn,
            "getappz/two",
            &p2.id,
            "/repo/two",
            "agentflare",
            Some("claude-code"),
            2,
        )
        .unwrap();

        let targets = bridge_targets(&conn);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].repo.to_string(), "getappz/one");
        assert_eq!(targets[0].project_id, p1.id);
        assert_eq!(targets[0].work_agent, None);
        assert_eq!(targets[1].repo.to_string(), "getappz/two");
        assert_eq!(targets[1].work_agent.as_deref(), Some("claude-code"));
    }
}
