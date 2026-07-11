//! Gated executor. The last line of defense: even a Safe-planned action is
//! re-verified against a fresh process snapshot immediately before the kill,
//! closing the window between planning and execution (TOCTOU).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::model::{identity_matches, Action, ActionKind, Lease, ProcInfo, Risk};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionOutcome {
    pub action: Action,
    pub executed: bool,
    pub detail: String,
}

/// Run planned actions through the gate.
///
/// - `execute == false`: pure dry run, nothing is ever killed.
/// - `execute == true`: only `KillTree` actions with risk `Safe` run, and only
///   after the lease identity is re-verified against `fresh_procs`.
///
/// `kill` performs the actual process-tree termination (injected for tests).
pub fn execute_actions(
    actions: &[Action],
    leases: &[Lease],
    fresh_procs: &HashMap<u32, ProcInfo>,
    tolerance_secs: u64,
    execute: bool,
    kill: &mut dyn FnMut(u32) -> eyre::Result<()>,
) -> Vec<ExecutionOutcome> {
    let mut outcomes = Vec::new();
    for action in actions {
        let mut executed = false;
        let detail = if !execute {
            "dry run".to_string()
        } else if action.kind != ActionKind::KillTree || action.risk != Risk::Safe {
            "gated: only Safe kill-tree actions are executable".to_string()
        } else {
            let lease_id = action.target.strip_prefix("lease:").unwrap_or("");
            let lease = leases.iter().find(|l| l.id == lease_id);
            match (lease, action.pid) {
                (Some(lease), Some(pid)) => match fresh_procs.get(&pid) {
                    Some(live)
                        if identity_matches(
                            &lease.identity,
                            &live.name,
                            live.start_time,
                            tolerance_secs,
                        ) =>
                    {
                        match kill(pid) {
                            Ok(()) => {
                                executed = true;
                                format!("killed process tree rooted at pid {pid}")
                            }
                            Err(err) => format!("kill failed: {err}"),
                        }
                    }
                    Some(live) => format!(
                        "refused: re-verification failed, pid {pid} is now '{}'",
                        live.name
                    ),
                    None => format!("refused: re-verification failed, pid {pid} already exited"),
                },
                _ => "refused: re-verification failed, lease no longer exists".to_string(),
            }
        };
        outcomes.push(ExecutionOutcome { action: action.clone(), executed, detail });
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ActionKind, Bucket, Identity, Risk};
    use pretty_assertions::assert_eq;

    fn proc(pid: u32, name: &str, start: u64) -> ProcInfo {
        ProcInfo {
            pid,
            ppid: Some(1),
            name: name.into(),
            cmd: name.into(),
            start_time: start,
            cpu_pct: 0.0,
            rss_bytes: 0,
            bucket: Bucket::Agents,
            protected: false,
        }
    }

    fn lease(id: &str, pid: u32, name: &str, start: u64) -> Lease {
        Lease {
            id: id.into(),
            pid,
            class: "agent".into(),
            created_at: 0,
            ttl_seconds: 10,
            identity: Identity { exe_name: name.into(), start_time: start },
            allow_kill: true,
        }
    }

    fn action(kind: ActionKind, target: &str, pid: u32, risk: Risk) -> Action {
        Action { kind, target: target.into(), reason: "test".into(), pid: Some(pid), risk }
    }

    fn run(
        actions: &[Action],
        leases: &[Lease],
        procs: HashMap<u32, ProcInfo>,
        execute: bool,
    ) -> (Vec<ExecutionOutcome>, Vec<u32>) {
        let mut killed = Vec::new();
        let outcomes = execute_actions(actions, leases, &procs, 2, execute, &mut |pid| {
            killed.push(pid);
            Ok(())
        });
        (outcomes, killed)
    }

    #[test]
    fn dry_run_never_kills() {
        let leases = [lease("l1", 10, "agent.exe", 700)];
        let procs = [(10, proc(10, "agent.exe", 700))].into_iter().collect();
        let actions = [action(ActionKind::KillTree, "lease:l1", 10, Risk::Safe)];
        let (outcomes, killed) = run(&actions, &leases, procs, false);
        assert_eq!(killed, Vec::<u32>::new());
        assert!(!outcomes[0].executed);
    }

    #[test]
    fn safe_verified_action_kills_on_execute() {
        let leases = [lease("l1", 10, "agent.exe", 700)];
        let procs = [(10, proc(10, "agent.exe", 700))].into_iter().collect();
        let actions = [action(ActionKind::KillTree, "lease:l1", 10, Risk::Safe)];
        let (outcomes, killed) = run(&actions, &leases, procs, true);
        assert_eq!(killed, vec![10]);
        assert!(outcomes[0].executed);
    }

    #[test]
    fn blocked_and_review_actions_never_execute_even_with_execute_flag() {
        let leases = [lease("l1", 10, "agent.exe", 700)];
        let procs = [(10, proc(10, "agent.exe", 700))].into_iter().collect();
        let actions = [
            action(ActionKind::Review, "lease:l1", 10, Risk::Blocked),
            action(ActionKind::Review, "lease:l1", 10, Risk::ReviewOnly),
        ];
        let (outcomes, killed) = run(&actions, &leases, procs, true);
        assert_eq!(killed, Vec::<u32>::new());
        assert!(outcomes.iter().all(|o| !o.executed));
    }

    #[test]
    fn kill_refused_when_fresh_snapshot_no_longer_matches() {
        // Planned Safe, but by execution time the pid belongs to another exe.
        let leases = [lease("l1", 10, "agent.exe", 700)];
        let procs = [(10, proc(10, "imposter.exe", 700))].into_iter().collect();
        let actions = [action(ActionKind::KillTree, "lease:l1", 10, Risk::Safe)];
        let (outcomes, killed) = run(&actions, &leases, procs, true);
        assert_eq!(killed, Vec::<u32>::new());
        assert!(!outcomes[0].executed);
        assert!(outcomes[0].detail.contains("re-verif"), "detail: {}", outcomes[0].detail);
    }

    #[test]
    fn kill_refused_when_process_exited_before_execution() {
        let leases = [lease("l1", 10, "agent.exe", 700)];
        let actions = [action(ActionKind::KillTree, "lease:l1", 10, Risk::Safe)];
        let (outcomes, killed) = run(&actions, &leases, HashMap::new(), true);
        assert_eq!(killed, Vec::<u32>::new());
        assert!(!outcomes[0].executed);
    }

    #[test]
    fn kill_refused_when_lease_vanished() {
        // Safe action references a lease that no longer exists.
        let procs = [(10, proc(10, "agent.exe", 700))].into_iter().collect();
        let actions = [action(ActionKind::KillTree, "lease:gone", 10, Risk::Safe)];
        let (outcomes, killed) = run(&actions, &[], procs, true);
        assert_eq!(killed, Vec::<u32>::new());
        assert!(!outcomes[0].executed);
    }
}
