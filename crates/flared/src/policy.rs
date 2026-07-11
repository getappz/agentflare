use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::model::{identity_matches, Action, ActionKind, Finding, Lease, ProcInfo, Risk};

/// Heuristic rule for spotting likely AI-tool orphans. Findings produced by
/// these rules are report-only — never automatic kills.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanRule {
    /// Regex matched against the process name.
    pub name_pattern: String,
    /// Only flag when the parent process is gone.
    pub require_dead_parent: bool,
    /// Minimum process age in seconds before it can be flagged.
    pub min_age_secs: u64,
}

/// Turn expired leases into actions. The safety core:
/// - identity verified against the live process -> KillTree, risk Safe
/// - pid gone -> no action (nothing to do; lease is pruned elsewhere)
/// - pid present but identity mismatch (PID reuse) -> Review, risk Blocked
/// - live process marked protected -> Review, risk Blocked
/// - lease without allow_kill -> Review, risk ReviewOnly
pub fn plan_lease_actions(
    expired: &[Lease],
    procs: &HashMap<u32, ProcInfo>,
    tolerance_secs: u64,
) -> Vec<Action> {
    let mut actions = Vec::new();
    for lease in expired {
        let target = format!("lease:{}", lease.id);
        let Some(live) = procs.get(&lease.pid) else {
            // Process already gone; nothing to kill, lease gets pruned.
            continue;
        };
        if live.protected {
            actions.push(Action {
                kind: ActionKind::Review,
                target,
                reason: format!("expired {} lease points at protected process '{}'", lease.class, live.name),
                pid: Some(lease.pid),
                risk: Risk::Blocked,
            });
            continue;
        }
        if !identity_matches(&lease.identity, &live.name, live.start_time, tolerance_secs) {
            actions.push(Action {
                kind: ActionKind::Review,
                target,
                reason: format!(
                    "pid {} no longer matches lease identity (now '{}'): likely PID reuse",
                    lease.pid, live.name
                ),
                pid: Some(lease.pid),
                risk: Risk::Blocked,
            });
            continue;
        }
        if !lease.allow_kill {
            actions.push(Action {
                kind: ActionKind::Review,
                target,
                reason: format!("expired {} lease (owner did not authorize kill)", lease.class),
                pid: Some(lease.pid),
                risk: Risk::ReviewOnly,
            });
            continue;
        }
        actions.push(Action {
            kind: ActionKind::KillTree,
            target,
            reason: format!("expired {} lease, identity verified", lease.class),
            pid: Some(lease.pid),
            risk: Risk::Safe,
        });
    }
    actions
}

/// Report-only orphan detection over the classified process table.
/// Protected processes are never flagged.
pub fn detect_orphans(
    procs: &HashMap<u32, ProcInfo>,
    rules: &[OrphanRule],
    now: u64,
) -> Vec<Finding> {
    let compiled: Vec<(regex::Regex, &OrphanRule)> = rules
        .iter()
        .filter_map(|rule| match regex::Regex::new(&rule.name_pattern) {
            Ok(re) => Some((re, rule)),
            Err(err) => {
                tracing::warn!(pattern = %rule.name_pattern, %err, "invalid orphan rule pattern, skipping");
                None
            }
        })
        .collect();

    let mut findings = Vec::new();
    for live in procs.values() {
        if live.protected {
            continue;
        }
        for (re, rule) in &compiled {
            if !re.is_match(&live.name) {
                continue;
            }
            let age = now.saturating_sub(live.start_time);
            if age < rule.min_age_secs {
                continue;
            }
            if rule.require_dead_parent {
                let parent_alive = live.ppid.is_some_and(|pp| procs.contains_key(&pp));
                if parent_alive {
                    continue;
                }
            }
            findings.push(Finding {
                pid: live.pid,
                name: live.name.clone(),
                reason: format!(
                    "matches orphan rule '{}' (age {age}s{})",
                    rule.name_pattern,
                    if rule.require_dead_parent { ", parent dead" } else { "" }
                ),
            });
            break;
        }
    }
    findings.sort_by_key(|f| f.pid);
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ActionKind, Bucket, Identity, Risk};
    use pretty_assertions::assert_eq;

    fn proc(pid: u32, ppid: Option<u32>, name: &str, start: u64) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            name: name.into(),
            cmd: name.into(),
            start_time: start,
            cpu_pct: 0.0,
            rss_bytes: 0,
            bucket: Bucket::Agents,
            protected: false,
        }
    }

    fn lease(pid: u32, name: &str, start: u64, allow_kill: bool) -> Lease {
        Lease {
            id: format!("l-{pid}"),
            pid,
            class: "agent".into(),
            created_at: 0,
            ttl_seconds: 10,
            identity: Identity { exe_name: name.into(), start_time: start },
            allow_kill,
        }
    }

    fn table(entries: Vec<ProcInfo>) -> HashMap<u32, ProcInfo> {
        entries.into_iter().map(|p| (p.pid, p)).collect()
    }

    #[test]
    fn verified_expired_lease_becomes_safe_kill() {
        let procs = table(vec![proc(10, Some(1), "agent.exe", 700)]);
        let actions = plan_lease_actions(&[lease(10, "agent.exe", 700, true)], &procs, 2);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, ActionKind::KillTree);
        assert_eq!(actions[0].risk, Risk::Safe);
        assert_eq!(actions[0].pid, Some(10));
    }

    #[test]
    fn pid_reuse_is_blocked_never_killed() {
        // Lease was for agent.exe, but pid 10 is now someone else's process.
        let procs = table(vec![proc(10, Some(1), "totally-different.exe", 9999)]);
        let actions = plan_lease_actions(&[lease(10, "agent.exe", 700, true)], &procs, 2);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, ActionKind::Review);
        assert_eq!(actions[0].risk, Risk::Blocked);
    }

    #[test]
    fn dead_pid_produces_no_action() {
        let procs = table(vec![]);
        let actions = plan_lease_actions(&[lease(10, "agent.exe", 700, true)], &procs, 2);
        assert_eq!(actions, vec![]);
    }

    #[test]
    fn protected_process_is_blocked_even_with_valid_lease() {
        let mut p = proc(10, Some(1), "agent.exe", 700);
        p.protected = true;
        let procs = table(vec![p]);
        let actions = plan_lease_actions(&[lease(10, "agent.exe", 700, true)], &procs, 2);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].risk, Risk::Blocked);
    }

    #[test]
    fn lease_without_allow_kill_is_review_only() {
        let procs = table(vec![proc(10, Some(1), "agent.exe", 700)]);
        let actions = plan_lease_actions(&[lease(10, "agent.exe", 700, false)], &procs, 2);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, ActionKind::Review);
        assert_eq!(actions[0].risk, Risk::ReviewOnly);
    }

    fn rule(pattern: &str, dead_parent: bool, min_age: u64) -> OrphanRule {
        OrphanRule {
            name_pattern: pattern.into(),
            require_dead_parent: dead_parent,
            min_age_secs: min_age,
        }
    }

    #[test]
    fn orphan_rule_flags_old_agent_with_dead_parent() {
        // ppid 999 is not in the table -> parent is dead.
        let procs = table(vec![proc(10, Some(999), "mcp-server.exe", 100)]);
        let findings = detect_orphans(&procs, &[rule("mcp-", true, 60)], 1000);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].pid, 10);
    }

    #[test]
    fn orphan_rule_skips_young_processes() {
        let procs = table(vec![proc(10, Some(999), "mcp-server.exe", 990)]);
        assert_eq!(detect_orphans(&procs, &[rule("mcp-", true, 60)], 1000), vec![]);
    }

    #[test]
    fn orphan_rule_skips_live_parent_when_dead_parent_required() {
        let procs = table(vec![
        	proc(1, None, "init", 0),
        	proc(10, Some(1), "mcp-server.exe", 100),
        ]);
        assert_eq!(detect_orphans(&procs, &[rule("mcp-", true, 60)], 1000), vec![]);
    }

    #[test]
    fn orphan_rule_never_flags_protected() {
        let mut p = proc(10, Some(999), "mcp-server.exe", 100);
        p.protected = true;
        let procs = table(vec![p]);
        assert_eq!(detect_orphans(&procs, &[rule("mcp-", true, 60)], 1000), vec![]);
    }
}
