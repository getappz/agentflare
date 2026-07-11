//! The sleeping sweep loop and the one-shot sweep it (and the CLI/API) run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::actions::{execute_actions, ExecutionOutcome};
use crate::config::Config;
use crate::events::EventLog;
use crate::janitor::lean_ctx::{check_registry, RegistryReport};
use crate::leases::LeaseStore;
use crate::model::{Action, Finding, ProcInfo};
use crate::policy::{detect_orphans, plan_lease_actions};
use crate::scanner::{scan, Pressure};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryOutcome {
    pub kind: String,
    pub path: String,
    pub report: RegistryReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepOutcome {
    pub ts: u64,
    pub pressure: Pressure,
    pub bucket_counts: HashMap<String, usize>,
    pub actions: Vec<Action>,
    pub outcomes: Vec<ExecutionOutcome>,
    pub orphans: Vec<Finding>,
    pub registries: Vec<RegistryOutcome>,
    pub lease_count: usize,
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Kill a whole process tree, cross-platform.
pub fn kill_process_tree(pid: u32) -> eyre::Result<()> {
    kill_tree::blocking::kill_tree(pid)
        .map(|_| ())
        .map_err(|err| eyre::eyre!("kill_tree({pid}): {err}"))
}

/// The production killer: reloads the lease store and re-queries the OS at
/// the moment of execution, so a lease heartbeated/removed after planning, or
/// a pid recycled after the scan, is never killed. Lease-backed kills require
/// an EXACT start-time match — fuzzy tolerance is for report-only heuristics.
pub fn make_verified_killer(store: &LeaseStore) -> impl FnMut(u32) -> eyre::Result<()> + '_ {
    move |pid| {
        let now = unix_now();
        let leases = store.load()?;
        let lease = leases
            .iter()
            .find(|l| l.pid == pid && l.allow_kill && l.expires_at() <= now)
            .ok_or_else(|| {
                eyre::eyre!("no expired kill-authorized lease for pid {pid} at execution time")
            })?;
        let (name, start) = crate::scanner::identity_of(pid)
            .ok_or_else(|| eyre::eyre!("pid {pid} exited before execution"))?;
        if !crate::model::identity_matches(&lease.identity, &name, start, 0) {
            eyre::bail!("identity changed for pid {pid} at execution time (now '{name}')");
        }
        kill_process_tree(pid)
    }
}

/// One full sweep:
///
/// 1. audit + classify processes
/// 2. drop leases whose pid is gone entirely
/// 3. plan actions for expired leases, run them through the gate
///    (`execute` decides whether Safe kills actually happen)
/// 4. deep sweeps additionally run orphan detection and registry checks
///    (both report-only here)
///
/// Every sweep appends a summary event to the ledger.
pub fn sweep_once(
    cfg: &Config,
    store: &LeaseStore,
    log: &EventLog,
    execute: bool,
    deep: bool,
    kill: &mut dyn FnMut(u32) -> eyre::Result<()>,
) -> eyre::Result<SweepOutcome> {
    let (procs, pressure) = scan(cfg);
    let now = unix_now();

    let mut leases = store.load()?;
    let before = leases.len();
    leases.retain(|l| procs.contains_key(&l.pid));
    if leases.len() != before {
        store.save(&leases)?;
    }

    let expired: Vec<_> = leases.iter().filter(|l| l.expires_at() <= now).cloned().collect();
    // Lease-backed kills demand an exact start-time match; the configured
    // tolerance applies only to report-only registry heuristics.
    let actions = plan_lease_actions(&expired, &procs, 0);
    let outcomes = execute_actions(&actions, &leases, &procs, 0, execute, kill);
    for outcome in outcomes.iter().filter(|o| o.executed) {
        if let Some(id) = outcome.action.target.strip_prefix("lease:") {
            let _ = store.remove(id);
        }
    }

    let orphans =
        if deep { detect_orphans(&procs, &cfg.orphan_rules, now) } else { Vec::new() };

    let mut registries = Vec::new();
    if deep {
        for reg in &cfg.registries {
            if reg.kind != "lean-ctx" || !reg.path.exists() {
                continue;
            }
            match check_registry(&reg.path, &procs, &reg.expected_exe, cfg.identity_tolerance_secs)
            {
                Ok(report) => registries.push(RegistryOutcome {
                    kind: reg.kind.clone(),
                    path: reg.path.display().to_string(),
                    report,
                }),
                Err(err) => {
                    tracing::warn!(path = %reg.path.display(), %err, "registry check failed")
                }
            }
        }
    }

    let mut bucket_counts: HashMap<String, usize> = HashMap::new();
    for p in procs.values() {
        *bucket_counts.entry(format!("{:?}", p.bucket).to_ascii_lowercase()).or_default() += 1;
    }
    let lease_count = store.load()?.len();

    let outcome = SweepOutcome {
        ts: now,
        pressure,
        bucket_counts,
        actions,
        outcomes,
        orphans,
        registries,
        lease_count,
    };
    log.append(
        "sweep",
        serde_json::json!({
            "deep": deep,
            "execute": execute,
            "pressure": outcome.pressure.level,
            "planned": outcome.actions.len(),
            "executed": outcome.outcomes.iter().filter(|o| o.executed).count(),
            "orphans": outcome.orphans.len(),
            "leases": outcome.lease_count,
        }),
    )?;
    Ok(outcome)
}

/// Snapshot shared between the sweep loop and the HTTP server.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub last: Option<SweepOutcome>,
    pub processes: Vec<ProcInfo>,
}

pub type SharedSnapshot = Arc<Mutex<Snapshot>>;

/// Run the always-on loop: light sweep every `light_interval_secs`, deep
/// sweep every `deep_interval_secs`, snapshot refreshed after each.
/// Lease enforcement is live (execute=true) — that is the daemon's contract.
pub async fn run_loop(
    cfg: Arc<Config>,
    store: Arc<LeaseStore>,
    log: Arc<EventLog>,
    snapshot: SharedSnapshot,
) {
    let light = std::time::Duration::from_secs(cfg.light_interval_secs.max(5));
    let deep_every =
        std::time::Duration::from_secs(cfg.deep_interval_secs.max(cfg.light_interval_secs.max(5)));
    // Deep sweeps are scheduled on elapsed time, not tick counts, so backoff
    // stretching the light cadence cannot stretch the deep cadence with it.
    let mut next_deep = tokio::time::Instant::now();
    loop {
        let deep = tokio::time::Instant::now() >= next_deep;
        if deep {
            next_deep = tokio::time::Instant::now() + deep_every;
        }
        let cfg2 = Arc::clone(&cfg);
        let store2 = Arc::clone(&store);
        let log2 = Arc::clone(&log);
        let result = tokio::task::spawn_blocking(move || {
            let (procs, _) = scan(&cfg2);
            let outcome = sweep_once(
                &cfg2,
                &store2,
                &log2,
                true,
                deep,
                &mut make_verified_killer(&store2),
            );
            (procs, outcome)
        })
        .await;
        match result {
            Ok((procs, Ok(outcome))) => {
                let mut snap = snapshot.lock().expect("snapshot lock");
                snap.processes = {
                    let mut list: Vec<ProcInfo> = procs.into_values().collect();
                    list.sort_by_key(|p| std::cmp::Reverse(p.rss_bytes));
                    list
                };
                snap.last = Some(outcome);
            }
            Ok((_, Err(err))) => tracing::warn!(%err, "sweep failed"),
            Err(err) => tracing::warn!(%err, "sweep task panicked"),
        }
        // Back off when everything is calm: green pressure doubles the nap,
        // but never sleep past the next deep-sweep deadline.
        let calm = {
            let snap = snapshot.lock().expect("snapshot lock");
            snap.last.as_ref().is_some_and(|o| o.pressure.level == "green")
        };
        let nap = if calm { light * 2 } else { light };
        let until_deep = next_deep
            .saturating_duration_since(tokio::time::Instant::now())
            .max(std::time::Duration::from_secs(1));
        tokio::time::sleep(nap.min(until_deep)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_sweep_on_real_system_is_safe_and_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let store = LeaseStore::new(dir.path());
        let log = EventLog::new(dir.path());
        let cfg = Config::default();
        let mut killed = Vec::new();
        let outcome = sweep_once(&cfg, &store, &log, false, true, &mut |pid| {
            killed.push(pid);
            Ok(())
        })
        .unwrap();
        assert!(killed.is_empty(), "dry run must never kill");
        assert!(outcome.ts > 0);
        assert!(!outcome.bucket_counts.is_empty(), "a real system has processes");
        // The sweep must have written its ledger line.
        assert!(!log.tail(1).unwrap().is_empty());
    }

    #[test]
    fn sweep_drops_leases_for_pids_that_no_longer_exist() {
        let dir = tempfile::tempdir().unwrap();
        let store = LeaseStore::new(dir.path());
        let log = EventLog::new(dir.path());
        let cfg = Config::default();
        // A pid that cannot exist: valid range but far beyond real tables.
        store
            .create(
                u32::MAX - 7,
                "test",
                60,
                crate::model::Identity { exe_name: "ghost.exe".into(), start_time: 1 },
                true,
                unix_now(),
            )
            .unwrap();
        let outcome =
            sweep_once(&cfg, &store, &log, false, false, &mut |_| Ok(())).unwrap();
        assert_eq!(outcome.lease_count, 0);
        assert!(store.load().unwrap().is_empty());
    }
}
