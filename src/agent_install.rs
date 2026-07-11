// Install/update/uninstall engine for `agentflare agents install|update|uninstall`.
// Each agent's package manager and package name come from agent_registry.rs.
// Dry-run mode prints commands instead of executing them.
use agent_registry::{self, AgentSpec};
use std::process::{Command, Stdio};

pub enum Outcome {
    Ok(String),
    Skipped(String),
    Err(String),
}

fn run_cmd(cmd: &str, args: &[&str], dry_run: bool) -> Outcome {
    let label = format!("{cmd} {}", args.join(" "));
    if dry_run {
        return Outcome::Ok(format!("[dry-run] would run: {label}"));
    }
    match Command::new(cmd)
        .args(args)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
    {
        Ok(s) if s.success() => Outcome::Ok(format!("{label} — ok")),
        Ok(s) => Outcome::Err(format!("{label} — exit code {}", s.code().unwrap_or(-1))),
        Err(e) => Outcome::Err(format!("{label} — {e}")),
    }
}

fn install_args(spec: &AgentSpec, version: Option<&str>) -> Option<Vec<String>> {
    let pm = spec.package_manager?;
    let pkg = spec.package_name?;
    match pm {
        "npm" => {
            let target = match version {
                Some(v) => format!("{pkg}@{v}"),
                None => pkg.to_string(),
            };
            Some(vec!["install".to_string(), "-g".to_string(), target])
        }
        "pip" => {
            let target = match version {
                Some(v) => format!("{pkg}=={v}"),
                None => pkg.to_string(),
            };
            Some(vec!["install".to_string(), target])
        }
        _ => None,
    }
}

fn uninstall_args(spec: &AgentSpec) -> Option<Vec<String>> {
    let pm = spec.package_manager?;
    let pkg = spec.package_name?;
    match pm {
        "npm" => Some(vec![
            "uninstall".to_string(),
            "-g".to_string(),
            pkg.to_string(),
        ]),
        "pip" => Some(vec![
            "uninstall".to_string(),
            "-y".to_string(),
            pkg.to_string(),
        ]),
        _ => None,
    }
}

pub fn run_install(
    registry: &[AgentSpec],
    agent: &str,
    version: Option<&str>,
    dry_run: bool,
) -> Outcome {
    let spec = match registry.iter().find(|s| s.id.as_str() == agent) {
        Some(s) => s,
        None => return Outcome::Err(format!("unknown agent: {agent}")),
    };
    if spec.tier != agent_registry::Tier::Cli {
        return Outcome::Skipped(format!(
            "{agent} is an editor extension, no binary to install"
        ));
    }
    let pm = match spec.package_manager {
        Some(p) => p,
        None => {
            return Outcome::Skipped(format!(
                "{agent} has no automated install — download from the official site"
            ));
        }
    };
    let args = match install_args(spec, version) {
        Some(a) => a,
        None => return Outcome::Err(format!("unsupported package manager: {pm}")),
    };
    let strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_cmd(pm, &strs, dry_run)
}

pub fn run_update(registry: &[AgentSpec], agent: &str, dry_run: bool) -> Outcome {
    let spec = match registry.iter().find(|s| s.id.as_str() == agent) {
        Some(s) => s,
        None => return Outcome::Err(format!("unknown agent: {agent}")),
    };
    if spec.tier != agent_registry::Tier::Cli {
        return Outcome::Skipped(format!("{agent} is an editor extension, nothing to update"));
    }
    let pm = match spec.package_manager {
        Some(p) => p,
        None => {
            return Outcome::Skipped(format!(
                "{agent} is manually installed — re-download from the official site"
            ));
        }
    };
    let pkg = match spec.package_name {
        Some(p) => p,
        None => return Outcome::Err(format!("no package name for {agent}")),
    };
    match pm {
        "npm" => run_cmd("npm", &["update", "-g", pkg], dry_run),
        "pip" => run_cmd("pip", &["install", "--upgrade", pkg], dry_run),
        _ => Outcome::Err(format!("unsupported package manager: {pm}")),
    }
}

pub fn run_uninstall(registry: &[AgentSpec], agent: &str, dry_run: bool) -> Outcome {
    let spec = match registry.iter().find(|s| s.id.as_str() == agent) {
        Some(s) => s,
        None => return Outcome::Err(format!("unknown agent: {agent}")),
    };
    if spec.tier != agent_registry::Tier::Cli {
        return Outcome::Skipped(format!(
            "{agent} is an editor extension, nothing to uninstall"
        ));
    }
    let pm = match spec.package_manager {
        Some(p) => p,
        None => {
            return Outcome::Skipped(format!(
                "{agent} is manually installed — remove from your system manually"
            ));
        }
    };
    let args = match uninstall_args(spec) {
        Some(a) => a,
        None => return Outcome::Err(format!("unsupported package manager: {pm}")),
    };
    let strs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_cmd(pm, &strs, dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_registry::{Agent, Tier};

    fn fake_registry() -> Vec<AgentSpec> {
        vec![
            AgentSpec {
                id: Agent::Aider,
                display_name: "aider",
                tier: Tier::Cli,
                binary_names: &[],
                version_args: &[],
                package_manager: Some("pip"),
                package_name: Some("aider-chat"),
            },
            AgentSpec {
                id: Agent::Cursor,
                display_name: "cursor",
                tier: Tier::Cli,
                binary_names: &[],
                version_args: &[],
                package_manager: None,
                package_name: None,
            },
            AgentSpec {
                id: Agent::Cline,
                display_name: "cline",
                tier: Tier::Extension,
                binary_names: &[],
                version_args: &[],
                package_manager: None,
                package_name: None,
            },
        ]
    }

    #[test]
    fn install_dry_run_prints_command() {
        let reg = fake_registry();
        let out = run_install(&reg, "aider", Some("1.0.0"), true);
        match out {
            Outcome::Ok(msg) => {
                assert!(msg.contains("[dry-run]"));
                assert!(msg.contains("aider-chat==1.0.0"));
            }
            _ => panic!("expected Ok with dry-run message"),
        }
    }

    #[test]
    fn install_without_version_pins_to_latest() {
        let reg = fake_registry();
        let out = run_install(&reg, "aider", None, true);
        match out {
            Outcome::Ok(msg) => {
                assert!(!msg.contains("=="));
                assert!(msg.contains("aider-chat"));
            }
            _ => panic!("expected Ok without version pin"),
        }
    }

    #[test]
    fn install_manual_agent_skips() {
        let reg = fake_registry();
        let out = run_install(&reg, "cursor", None, true);
        match out {
            Outcome::Skipped(msg) => assert!(msg.contains("no automated install")),
            _ => panic!("expected Skipped"),
        }
    }

    #[test]
    fn install_extension_agent_skips() {
        let reg = fake_registry();
        let out = run_install(&reg, "cline", None, true);
        match out {
            Outcome::Skipped(msg) => assert!(msg.contains("editor extension")),
            _ => panic!("expected Skipped"),
        }
    }

    #[test]
    fn install_unknown_agent_errors() {
        let reg = fake_registry();
        let out = run_install(&reg, "nonexistent", None, true);
        match out {
            Outcome::Err(msg) => assert!(msg.contains("unknown agent")),
            _ => panic!("expected Err"),
        }
    }

    #[test]
    fn uninstall_dry_run_prints_command() {
        let reg = fake_registry();
        let out = run_uninstall(&reg, "aider", true);
        match out {
            Outcome::Ok(msg) => {
                assert!(msg.contains("[dry-run]"));
                assert!(msg.contains("uninstall"));
            }
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn update_dry_run_prints_command() {
        let reg = fake_registry();
        let out = run_update(&reg, "aider", true);
        match out {
            Outcome::Ok(msg) => {
                assert!(msg.contains("[dry-run]"));
                assert!(msg.contains("--upgrade"));
            }
            _ => panic!("expected Ok"),
        }
    }
}
