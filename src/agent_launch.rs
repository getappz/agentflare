// Launch engine for `agentflare agents launch <agent> [args...]`.
// Finds the agent binary on PATH, maps --model/--mode to agent-native
// flags, and executes with pass-through args and inherited stdio.
use agent_registry::detect::find_binary;
use agent_registry::{AgentSpec, Tier};
use std::process::{Command, Stdio};

pub enum LaunchOutcome {
    Launched,
    NotFound(String),
    UnknownAgent(String),
    Extension(String),
}

pub fn run_launch(
    registry: &[AgentSpec],
    agent: &str,
    model: Option<&str>,
    mode: Option<&str>,
    args: &[String],
) -> LaunchOutcome {
    let spec = match registry.iter().find(|s| s.id.as_str() == agent) {
        Some(s) => s,
        None => return LaunchOutcome::UnknownAgent(agent.to_string()),
    };

    if spec.tier != Tier::Cli {
        return LaunchOutcome::Extension(format!(
            "{agent} is an editor extension — no binary to launch"
        ));
    }

    let binary = match find_binary(spec.binary_names) {
        Some(p) => p,
        None => {
            return LaunchOutcome::NotFound(format!(
                "{} not found on PATH — install it first with: agentflare agents install {agent}",
                spec.binary_names.join(" / ")
            ));
        }
    };

    let mut cmd = Command::new(&binary);
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    cmd.stdin(Stdio::inherit());

    if let Some(m) = model {
        cmd.arg("--model").arg(m);
    }
    if let Some(m) = mode {
        cmd.arg("--mode").arg(m);
    }
    for a in args {
        cmd.arg(a);
    }

    match cmd.status() {
        Ok(s) if s.success() => LaunchOutcome::Launched,
        Ok(s) => {
            let code = s.code().unwrap_or(-1);
            std::process::exit(code);
        }
        Err(e) => LaunchOutcome::NotFound(format!(
            "failed to launch {}: {e}",
            binary.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_registry::{Agent, Tier};

    fn test_registry() -> Vec<AgentSpec> {
        vec![
            AgentSpec {
                id: Agent::Aider,
                display_name: "aider",
                tier: Tier::Cli,
                binary_names: &["aider"],
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
    fn launch_unknown_agent_errors() {
        let reg = test_registry();
        match run_launch(&reg, "nonexistent", None, None, &[]) {
            LaunchOutcome::UnknownAgent(msg) => assert!(msg.contains("nonexistent")),
            _ => panic!("expected UnknownAgent"),
        }
    }

    #[test]
    fn launch_extension_agent_errors() {
        let reg = test_registry();
        match run_launch(&reg, "cline", None, None, &[]) {
            LaunchOutcome::Extension(msg) => assert!(msg.contains("editor extension")),
            _ => panic!("expected Extension"),
        }
    }

    #[test]
    fn launch_not_on_path_errors() {
        let reg = test_registry();
        // "aider" unlikely to be on a test PATH
        match run_launch(&reg, "aider", None, None, &[]) {
            LaunchOutcome::NotFound(msg) => assert!(msg.contains("not found on PATH")),
            _ => {}
        }
    }
}
