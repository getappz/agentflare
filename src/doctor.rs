// `agentflare doctor` — read-only setup/config health check. Distinct from
// `agentflare agents doctor` (agent BINARY/version detection) and
// `agentflare git doctor` (claim-worktree hygiene): this checks whether
// agentflare's OWN config (MCP registration, usage rules) is actually wired
// into each coding agent, reusing the exact `Component::check` closures
// `init` already runs, just without ever calling `apply`.
use crate::components::{get_components, rule_targets};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub host: String,
    pub component_id: &'static str,
    pub ok: bool,
    pub describe: String,
}

#[derive(Debug, Serialize)]
pub struct StaleRule {
    pub host: String,
    pub path: PathBuf,
}

pub(crate) fn check_host(host: &str) -> Vec<CheckResult> {
    get_components(host)
        .into_iter()
        .map(|c| CheckResult {
            host: host.to_string(),
            component_id: c.id,
            ok: (c.check)(),
            describe: c.describe,
        })
        .collect()
}

pub(crate) fn stale_rules_for_host(host: &str) -> Vec<StaleRule> {
    rule_targets(host)
        .into_iter()
        .filter(|(path, current)| crate::init::is_stale_rule(path, current))
        .map(|(path, _)| StaleRule {
            host: host.to_string(),
            path,
        })
        .collect()
}

/// Hosts to check when `--agent` isn't given: every agent binary actually
/// detected on PATH (same detection `agentflare agents` uses) — avoids
/// reporting on config for tools the user doesn't have installed. Version
/// resolution accuracy doesn't matter here (only which agents are present),
/// so a throwaway cache is fine.
fn default_hosts() -> Vec<String> {
    let mut cache = std::collections::HashMap::new();
    agent_registry::detect::detect_all(agent_registry::REGISTRY, &mut cache)
        .into_iter()
        .map(|a| a.id.to_string())
        .collect()
}

pub fn run(agent: Option<&str>, json: bool) {
    let hosts: Vec<String> = match agent {
        Some(a) => vec![a.to_string()],
        None => default_hosts(),
    };

    if hosts.is_empty() {
        eprintln!(
            "doctor: no coding agents detected on PATH; pass --agent <agent> to check one directly"
        );
        return;
    }

    let mut checks = Vec::new();
    let mut stale = Vec::new();
    for host in &hosts {
        checks.extend(check_host(host));
        stale.extend(stale_rules_for_host(host));
    }

    let healthy = checks.iter().all(|r| r.ok) && stale.is_empty();

    if json {
        #[derive(Serialize)]
        struct Report<'a> {
            checks: &'a [CheckResult],
            stale_rules: &'a [StaleRule],
            healthy: bool,
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&Report {
                checks: &checks,
                stale_rules: &stale,
                healthy,
            })
            .unwrap()
        );
    } else {
        let total = checks.len();
        let passed = checks.iter().filter(|r| r.ok).count();

        println!("agentflare doctor\n");
        for host in &hosts {
            let host_checks: Vec<&CheckResult> =
                checks.iter().filter(|r| &r.host == host).collect();
            let host_stale: Vec<&StaleRule> = stale.iter().filter(|s| &s.host == host).collect();
            let host_ok = host_checks.iter().filter(|r| r.ok).count();
            let host_total = host_checks.len();
            let host_healthy = host_ok == host_total && host_stale.is_empty();
            let mark = if host_healthy { "✓" } else { "✗" };

            println!("{mark} {host:16} {host_ok}/{host_total} ok");
            for r in host_checks.iter().filter(|r| !r.ok) {
                println!("    ✗ {:22} {}", r.component_id, r.describe);
            }
            for s in &host_stale {
                println!("    ⚠ stale rule: {}", s.path.display());
            }
        }

        println!();
        if healthy {
            println!("{passed}/{total} checks passed — all good.");
        } else {
            println!("{passed}/{total} checks passed — run `agentflare init` to fix.");
        }
    }

    if !healthy {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::rule_targets;
    use crate::paths::test_support::with_temp_home;

    #[test]
    fn check_host_reports_rules_missing_when_absent() {
        with_temp_home(|| {
            let results = check_host("claude-code");
            let rules = results
                .iter()
                .find(|r| r.component_id == "rules")
                .expect("claude-code should have a 'rules' component");
            assert!(!rules.ok);
        });
    }

    #[test]
    fn check_host_reports_rules_ok_once_targets_are_written() {
        with_temp_home(|| {
            for (path, content) in rule_targets("claude-code") {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, content).unwrap();
            }
            let results = check_host("claude-code");
            let rules = results
                .iter()
                .find(|r| r.component_id == "rules")
                .expect("claude-code should have a 'rules' component");
            assert!(rules.ok);
        });
    }

    #[test]
    fn stale_rules_for_host_detects_known_old_wording() {
        with_temp_home(|| {
            let targets = rule_targets("claude-code");
            for (path, content) in &targets {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, content).unwrap();
            }
            // Overwrite the exa.md target with its known-superseded wording.
            let (exa_path, _) = targets
                .iter()
                .find(|(p, _)| p.file_name().unwrap() == "exa.md")
                .unwrap();
            std::fs::write(exa_path, crate::rule_text::EXA_SUPERSEDED[0]).unwrap();

            let stale = stale_rules_for_host("claude-code");
            assert!(
                stale.iter().any(|s| s.path == *exa_path),
                "exa.md with superseded content should be flagged stale"
            );
        });
    }

    #[test]
    fn stale_rules_for_host_is_empty_when_content_is_current() {
        with_temp_home(|| {
            for (path, content) in rule_targets("claude-code") {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, content).unwrap();
            }
            assert!(stale_rules_for_host("claude-code").is_empty());
        });
    }
}
