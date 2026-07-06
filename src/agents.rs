// CLI rendering for `agentflare agents list` / `agentflare agents doctor`.
// Kept separate from agent_detect.rs so the detection engine stays free of
// println!/format concerns and is fully unit-testable in isolation.
use crate::agent_detect::{self, DetectedAgent, VersionRunner};
use crate::agent_install::{self, Outcome};
use crate::agent_registry::{self, AgentSpec};
use crate::state;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Serialize)]
struct ListOutput {
    agents: Vec<AgentRow>,
}

#[derive(Serialize)]
struct AgentRow {
    agent: String,
    version: Option<String>,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    binary_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn columns() -> (&'static str, &'static str, &'static str) {
    ("AGENT", "VERSION", "STATUS")
}

fn pad(s: &str, width: usize) -> String {
    if s.len() >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - s.len()))
    }
}

fn render_table(agents: &[DetectedAgent]) {
    if agents.is_empty() {
        println!("No agents detected.");
        return;
    }
    let (c0, c1, c2) = columns();
    let max_agent = agents
        .iter()
        .map(|a| a.display_name.len())
        .max()
        .unwrap_or(0)
        .max(c0.len());
    let max_version = agents
        .iter()
        .map(|a| a.version.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(0)
        .max(c1.len());

    println!(
        "  {}  {}  {}",
        pad(c0, max_agent),
        pad(c1, max_version),
        c2
    );
    for a in agents {
        println!(
            "  {}  {}  {}",
            pad(a.display_name, max_agent),
            pad(a.version.as_deref().unwrap_or("-"), max_version),
            a.status
        );
    }
}

fn to_rows(agents: &[DetectedAgent]) -> Vec<AgentRow> {
    agents
        .iter()
        .map(|a| AgentRow {
            agent: a.display_name.to_string(),
            version: a.version.clone(),
            status: a.status.to_string(),
            binary_path: Some(a.binary_path.clone()),
            error: a.error.clone(),
        })
        .collect()
}

pub fn run_list(
    registry: &[AgentSpec],
    cache: &mut HashMap<String, state::VersionCacheEntry>,
    runner: &dyn VersionRunner,
    json: bool,
) {
    let agents = agent_detect::detect_all_with(registry, cache, runner);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ListOutput {
                agents: to_rows(&agents)
            })
            .unwrap()
        );
    } else {
        render_table(&agents);
    }
}

pub fn run_doctor(
    registry: &[AgentSpec],
    cache: &mut HashMap<String, state::VersionCacheEntry>,
    runner: &dyn VersionRunner,
    json: bool,
) {
    let agents = agent_detect::detect_all_with(registry, cache, runner);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ListOutput {
                agents: to_rows(&agents)
            })
            .unwrap()
        );
    } else {
        render_table(&agents);
        for a in &agents {
            if a.status == "unknown" {
                if let Some(ref err) = a.error {
                    println!(
                        "  {}: {} — {}",
                        a.display_name, a.binary_path, err
                    );
                }
            }
        }
    }
}

pub fn cli_list(json: bool) {
    let mut state = state::load();
    run_list(
        agent_registry::REGISTRY,
        &mut state.version_cache,
        &agent_detect::RealVersionRunner,
        json,
    );
    state::save(&state);
}

pub fn cli_doctor(json: bool) {
    let mut state = state::load();
    run_doctor(
        agent_registry::REGISTRY,
        &mut state.version_cache,
        &agent_detect::RealVersionRunner,
        json,
    );
    state::save(&state);
}

pub fn cli_install(agent: &str, dry_run: bool) {
    match agent_install::run_install(agent_registry::REGISTRY, agent, None, dry_run) {
        Outcome::Ok(msg) => println!("{msg}"),
        Outcome::Skipped(msg) => println!("skip: {msg}"),
        Outcome::Err(msg) => eprintln!("error: {msg}"),
    }
}

pub fn cli_update(agent: &str, dry_run: bool) {
    match agent_install::run_update(agent_registry::REGISTRY, agent, dry_run) {
        Outcome::Ok(msg) => println!("{msg}"),
        Outcome::Skipped(msg) => println!("skip: {msg}"),
        Outcome::Err(msg) => eprintln!("error: {msg}"),
    }
}

pub fn cli_uninstall(agent: &str, dry_run: bool) {
    match agent_install::run_uninstall(agent_registry::REGISTRY, agent, dry_run) {
        Outcome::Ok(msg) => println!("{msg}"),
        Outcome::Skipped(msg) => println!("skip: {msg}"),
        Outcome::Err(msg) => eprintln!("error: {msg}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_registry::{Agent, Tier};
    use std::path::Path;

    struct FakeRunner {
        response: Result<String, String>,
    }

    impl VersionRunner for FakeRunner {
        fn run(&self, _binary: &Path, _args: &[&str]) -> Result<String, String> {
            self.response.clone()
        }
    }

    fn with_temp_path_dir(f: impl FnOnce(&Path)) {
        let _guard = crate::agent_detect::PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir()
            .join(format!("agentflare-test-agents-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", &dir);
        f(&dir);
        match original {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_registry() -> [AgentSpec; 1] {
        [AgentSpec {
            id: Agent::Aider,
            display_name: "test-agent",
            tier: Tier::Cli,
            binary_names: &["testbin"],
            version_args: &["--version"],
            package_manager: None,
            package_name: None,
        }]
    }

    #[test]
    fn list_json_output_contains_expected_fields() {
        with_temp_path_dir(|dir| {
            std::fs::write(dir.join("testbin"), "").unwrap();
            let runner = FakeRunner {
                response: Ok("1.2.3".to_string()),
            };
            let mut cache = HashMap::new();

            let detected = agent_detect::detect_all_with(
                &test_registry(),
                &mut cache,
                &runner,
            );
            let output = ListOutput { agents: to_rows(&detected) };
            let json = serde_json::to_string_pretty(&output).unwrap();
            assert!(json.contains("\"agent\": \"test-agent\""));
            assert!(json.contains("\"version\": \"1.2.3\""));
            assert!(json.contains("\"status\": \"ready\""));
            assert!(json.contains("\"binary_path\""));
            assert!(!json.contains("\"error\""));
        });
    }

    #[test]
    fn list_json_includes_error_for_unknown_status() {
        with_temp_path_dir(|dir| {
            std::fs::write(dir.join("broken-bin"), "").unwrap();
            let runner = FakeRunner {
                response: Err("failed: timeout".to_string()),
            };
            let mut cache = HashMap::new();

            let registry = [AgentSpec {
                id: Agent::Aider,
                display_name: "broken-agent",
                tier: Tier::Cli,
                binary_names: &["broken-bin"],
                version_args: &["--version"],
                package_manager: None,
                package_name: None,
            }];
            let detected = agent_detect::detect_all_with(
                &registry,
                &mut cache,
                &runner,
            );
            let output = ListOutput { agents: to_rows(&detected) };
            let json = serde_json::to_string_pretty(&output).unwrap();
            assert!(json.contains("\"status\": \"unknown\""));
            assert!(json.contains("\"error\""));
            assert!(json.contains("\"failed: timeout\""));
        });
    }

    #[test]
    fn list_empty_output_when_no_cli_agents_on_path() {
        let empty_registry: [AgentSpec; 0] = [];
        let mut cache = HashMap::new();
        let runner = FakeRunner {
            response: Ok("1.0".to_string()),
        };

        let detected = agent_detect::detect_all_with(&empty_registry, &mut cache, &runner);
        assert!(detected.is_empty());
    }

    #[test]
    fn table_empty_agents_prints_no_agents_message() {
        let registry: [AgentSpec; 0] = [];
        let mut cache = HashMap::new();
        let stub = FakeRunner {
            response: Ok("1.0".to_string()),
        };

        run_list(&registry, &mut cache, &stub, false);
    }

    #[test]
    fn doctor_runs_without_panicking() {
        with_temp_path_dir(|dir| {
            std::fs::write(dir.join("docbin"), "").unwrap();
            let runner = FakeRunner {
                response: Ok("4.5.6".to_string()),
            };
            let mut cache = HashMap::new();

            let registry = [AgentSpec {
                id: Agent::Aider,
                display_name: "doc-agent",
                tier: Tier::Cli,
                binary_names: &["docbin"],
                version_args: &["--version"],
                package_manager: None,
                package_name: None,
            }];
            run_doctor(&registry, &mut cache, &runner, false);
        });
    }

    #[test]
    fn doctor_json_emits_same_structure_as_list() {
        with_temp_path_dir(|dir| {
            std::fs::write(dir.join("drbin"), "").unwrap();
            let runner = FakeRunner {
                response: Ok("7.8.9".to_string()),
            };
            let mut cache = HashMap::new();

            let registry = [AgentSpec {
                id: Agent::Aider,
                display_name: "dr-agent",
                tier: Tier::Cli,
                binary_names: &["drbin"],
                version_args: &["--version"],
                package_manager: None,
                package_name: None,
            }];
            let detected = agent_detect::detect_all_with(
                &registry,
                &mut cache,
                &runner,
            );
            let output = ListOutput { agents: to_rows(&detected) };
            let json = serde_json::to_string_pretty(&output).unwrap();
            assert!(json.contains("\"agent\": \"dr-agent\""));
            assert!(json.contains("\"version\": \"7.8.9\""));
        });
    }
}
