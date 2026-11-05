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
///
/// Propagates items-DB read errors so the loop can drop a bad held connection
/// instead of treating corruption / I/O failure as "no repos registered".
fn bridge_targets(conn: &rusqlite::Connection) -> Result<Vec<BridgeTarget>, String> {
    let mut targets: Vec<BridgeTarget> = agentflare_backend::bridge_repo::list(conn)
        .map_err(|e| e.to_string())?
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
            return Ok(targets);
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
    Ok(targets)
}

/// Shared-handle failures must abort the rest of the targets loop; other
/// hard errors stay per-repo so one bad GitHub response does not stall the rest.
fn should_abort_targets_loop(err: &str) -> bool {
    is_bad_items_handle(err) || is_credential_failure(err)
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
///
/// Returns the failure instead of printing it: the caller runs this every
/// tick, and only it knows whether the failure is new.
fn open_items_db() -> Result<rusqlite::Connection, String> {
    let path = crate::paths::agentflare_dir().join("backend.db");
    agentflare_backend::db::open_db(&path)
        .map_err(|e| format!("item database unavailable at {} ({e})", path.display()))
}

/// Sleep after consecutive failures: `interval * 2^min(failures, 4)`.
fn backoff_delay(interval: std::time::Duration, failures: u32) -> std::time::Duration {
    interval * 2u32.pow(failures.min(4))
}

/// After a tick that already held handles: reset the counter on success, else
/// bump it and return the backoff sleep for the current (pre-bump) count.
fn after_tick(
    failures: u32,
    tick_ok: bool,
    interval: std::time::Duration,
) -> (u32, std::time::Duration) {
    if tick_ok {
        (0, interval)
    } else {
        (
            failures.saturating_add(1),
            backoff_delay(interval, failures),
        )
    }
}

/// `run_once` hard-errors are `GitHubError` strings; sqlite failures from the
/// items DB usually arrive as `response parse error: <rusqlite>`. A broken or
/// replaced file leaves the held connection unusable until we drop it.
fn is_bad_items_handle(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("disk i/o")
        || lower.contains("not a database")
        || lower.contains("readonly database")
        || lower.contains("unable to open database")
        || lower.contains("cannot open")
}

/// Credential failures that somehow surface as hard `run_once` errors (soft
/// `NoAuth` stays `Ok` with `soft_error`). Drop the cached client so the next
/// tick re-resolves credentials.
fn is_credential_failure(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("no usable github credentials")
        || lower.contains("rejected the credential")
        || lower.contains("github_token")
        || (lower.contains("credential") && lower.contains("401"))
}

/// Clear held handles when a tick error means they cannot be reused.
fn invalidate_handles_for_tick_err(
    err: &str,
    conn: &mut Option<rusqlite::Connection>,
    client: &mut Option<crate::github::Client>,
) -> bool {
    let mut invalidated = false;
    if is_bad_items_handle(err) {
        *conn = None;
        invalidated = true;
    }
    if is_credential_failure(err) {
        *client = None;
        invalidated = true;
    }
    invalidated
}

fn run_forever(config: BridgeConfig) {
    let interval = std::time::Duration::from_secs(config.interval_secs);
    eprintln!(
        "github bridge: started poll loop (every {}s)",
        config.interval_secs
    );

    // Soft errors repeat every tick by nature, so log a CHANGE per repo
    // rather than each occurrence: 1440 identical lines a day would bury the
    // one thing worth reading, and silence would hide a `Forbidden` — a repo
    // that is permanently unreachable and never says so. Keyed by repo since
    // one daemon now polls several independently.
    let mut last_soft: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut had_targets = true; // forces the "nothing registered" message on the first empty tick
    // Handles are opened once and kept across ticks; a failure to (re)acquire
    // one — or a failed run_once — backs off exponentially (interval -> 16x
    // interval) instead of hammering every interval, and resets on success.
    // A bad items-DB handle (replaced file, I/O error, …) is dropped so the
    // next tick reopens it; same for a hard credential failure on the client.
    let mut conn: Option<rusqlite::Connection> = None;
    let mut client: Option<crate::github::Client> = None;
    let mut failures: u32 = 0;
    let mut last_db_err: Option<String> = None;
    let mut logged_no_client = false;

    loop {
        if conn.is_none() {
            match open_items_db() {
                Ok(c) => {
                    last_db_err = None;
                    conn = Some(c);
                }
                Err(e) => {
                    // Log on change, not once: a lock error followed by a
                    // schema-ahead error needs its rebuild instructions shown.
                    if last_db_err.as_deref() != Some(e.as_str()) {
                        eprintln!("github bridge: {e}; waiting for it to become available...");
                        last_db_err = Some(e);
                    }
                }
            }
        }
        if conn.is_some() && client.is_none() {
            match crate::github::Client::new() {
                Ok(c) => {
                    logged_no_client = false;
                    client = Some(c);
                }
                Err(e) => {
                    if !logged_no_client {
                        eprintln!("github bridge: waiting for usable credential ({e})...");
                        logged_no_client = true;
                    }
                }
            }
        }
        let mut tick_ok = true;
        let mut tick_err: Option<String> = None;
        {
            let (Some(conn_ref), Some(client_ref)) = (conn.as_ref(), client.as_ref()) else {
                std::thread::sleep(backoff_delay(interval, failures));
                failures = failures.saturating_add(1);
                continue;
            };
            let targets = match bridge_targets(conn_ref) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!(
                        "github bridge: reading registered repos failed ({e}); reopening items db"
                    );
                    tick_ok = false;
                    // Prefix so is_bad_items_handle clears the retained connection.
                    tick_err = Some(format!("unable to open database: {e}"));
                    Vec::new()
                }
            };
            if tick_err.is_none() && targets.is_empty() {
                if had_targets {
                    eprintln!(
                        "github bridge: no repos registered — link a project from inside a repo \
                         (any agentflare CLI/MCP call there) or set AGENTFLARE_BRIDGE_REPO"
                    );
                    had_targets = false;
                }
                // Acquire succeeded; nothing to poll is not a failure.
                failures = 0;
                std::thread::sleep(interval);
                continue;
            }
            if tick_err.is_none() {
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
                        client: client_ref.clone(),
                        repo: target.repo,
                        config: repo_config,
                        project_id: target.project_id,
                        ledger,
                    };
                    let now = crate::claims::now();
                    let repo_key = ctx.repo.to_string();
                    match run_once(&ctx, conn_ref, now) {
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
                                        eprintln!(
                                            "github bridge: {repo_key} ticks are ending early: {e}"
                                        );
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
                        Err(e) => {
                            eprintln!("github bridge: {repo_key} tick failed: {e}");
                            tick_ok = false;
                            let abort = should_abort_targets_loop(&e);
                            tick_err = Some(e);
                            if abort {
                                // Shared handle is unusable for the rest of this tick;
                                // Options are cleared after this borrow ends.
                                break;
                            }
                        }
                    }
                }
            }
        }
        if let Some(e) = tick_err.as_deref() {
            let _ = invalidate_handles_for_tick_err(e, &mut conn, &mut client);
        }
        let (next_failures, sleep_for) = after_tick(failures, tick_ok, interval);
        failures = next_failures;
        std::thread::sleep(sleep_for);
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

        let targets = bridge_targets(&conn).expect("list should succeed on a migrated db");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].repo.to_string(), "getappz/one");
        assert_eq!(targets[0].project_id, p1.id);
        assert_eq!(targets[0].work_agent, None);
        assert_eq!(targets[1].repo.to_string(), "getappz/two");
        assert_eq!(targets[1].work_agent.as_deref(), Some("claude-code"));
    }

    #[test]
    fn bridge_targets_surfaces_list_errors_instead_of_empty() {
        // Bare connection has no bridge_repos table — list must Err, not [].
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let err = match bridge_targets(&conn) {
            Err(e) => e,
            Ok(_) => panic!("missing schema is a read failure, not an empty list"),
        };
        assert!(
            err.to_ascii_lowercase().contains("no such table")
                || err.to_ascii_lowercase().contains("bridge_repos"),
            "unexpected list error: {err}"
        );
        // Runner prefixes this so invalidate_handles_for_tick_err drops conn.
        let as_tick_err = format!("unable to open database: {err}");
        let mut held: Option<rusqlite::Connection> = rusqlite::Connection::open_in_memory().ok();
        let mut client: Option<crate::github::Client> = None;
        assert!(invalidate_handles_for_tick_err(
            &as_tick_err,
            &mut held,
            &mut client
        ));
        assert!(held.is_none());
    }

    #[test]
    fn bad_items_handle_errors_are_detected() {
        assert!(is_bad_items_handle("response parse error: disk I/O error"));
        assert!(is_bad_items_handle("file is not a database"));
        assert!(is_bad_items_handle(
            "response parse error: attempt to write a readonly database"
        ));
        assert!(is_bad_items_handle("unable to open database file"));
        assert!(is_bad_items_handle("Cannot open db"));
        // Busy/locked is transient; keep the held connection.
        assert!(!is_bad_items_handle("database is locked"));
        assert!(!is_bad_items_handle("GitHub HTTP 500: boom"));
    }

    #[test]
    fn bad_handle_clears_conn_so_next_tick_reacquires() {
        // Real rusqlite NOTADB display, as run_once wraps backend errors.
        let sqlite_err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOTADB),
            Some("file is not a database".into()),
        );
        let as_tick_err = format!("response parse error: {sqlite_err}");
        let mut conn: Option<rusqlite::Connection> = rusqlite::Connection::open_in_memory().ok();
        let mut client: Option<crate::github::Client> = None;
        assert!(conn.is_some());
        assert!(invalidate_handles_for_tick_err(
            &as_tick_err,
            &mut conn,
            &mut client
        ));
        assert!(
            conn.is_none(),
            "bad handle must clear conn so the next tick reopens"
        );
        // No client was held; sqlite errors must not be mistaken for auth.
        assert!(!is_credential_failure(&as_tick_err));
        assert!(client.is_none());
    }

    #[test]
    fn credential_failure_clears_client_not_conn() {
        let mut conn: Option<rusqlite::Connection> = rusqlite::Connection::open_in_memory().ok();
        let mut client: Option<crate::github::Client> = None;
        let err = "GitHub rejected the credential (401). Refresh it: 'gh auth login' or reset GITHUB_TOKEN.";
        assert!(is_credential_failure(err));
        assert!(!is_bad_items_handle(err));
        assert!(invalidate_handles_for_tick_err(err, &mut conn, &mut client));
        assert!(conn.is_some(), "auth failure must not drop the items db");
        assert!(client.is_none());
    }

    #[test]
    fn tick_failures_back_off_and_reset_on_success() {
        let interval = std::time::Duration::from_secs(1);
        let (f, d) = after_tick(0, false, interval);
        assert_eq!((f, d), (1, interval));
        let (f, d) = after_tick(1, false, interval);
        assert_eq!((f, d), (2, interval * 2));
        let (f, d) = after_tick(2, false, interval);
        assert_eq!((f, d), (3, interval * 4));
        let (f, d) = after_tick(4, false, interval);
        assert_eq!((f, d), (5, interval * 16));
        // Cap at 2^4 even as the counter grows.
        let (f, d) = after_tick(9, false, interval);
        assert_eq!((f, d), (10, interval * 16));
        // Success resets regardless of prior streak.
        let (f, d) = after_tick(9, true, interval);
        assert_eq!((f, d), (0, interval));
        assert_eq!(
            backoff_delay(interval, 3),
            interval * 8,
            "acquire path uses the same schedule"
        );
    }

    #[test]
    fn only_shared_handle_errors_abort_the_targets_loop() {
        assert!(should_abort_targets_loop(
            "response parse error: file is not a database"
        ));
        assert!(should_abort_targets_loop(
            "GitHub rejected the credential (401). Refresh it."
        ));
        // Repo-local / HTTP failures must not skip remaining targets.
        assert!(!should_abort_targets_loop("GitHub HTTP 500: boom"));
        assert!(!should_abort_targets_loop("database is locked"));
    }
}
