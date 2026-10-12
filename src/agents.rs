// CLI rendering for `agentflare agents list` / `agentflare agents doctor`.
// Kept separate from agent_detect.rs so the detection engine stays free of
// println!/format concerns and is fully unit-testable in isolation.
use crate::agent_install::{self, Outcome};
use crate::agent_launch::{self, LaunchOutcome};
use crate::state;
use agent_registry::detect::{self, DetectedAgent, VersionRunner};
use agent_registry::{self, AgentSpec};
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

/// `ready` only for an installed agent that can take work: an agent that is
/// cooling down or over its usage threshold shows that instead.
fn display_status(a: &DetectedAgent) -> String {
    match (a.status, crate::quota::failover::availability_state(a.id)) {
        ("ready", Some(state)) => state,
        (status, _) => status.to_string(),
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

    println!("  {}  {}  {}", pad(c0, max_agent), pad(c1, max_version), c2);
    for a in agents {
        println!(
            "  {}  {}  {}",
            pad(a.display_name, max_agent),
            pad(a.version.as_deref().unwrap_or("-"), max_version),
            display_status(a)
        );
    }
}

fn to_rows(agents: &[DetectedAgent]) -> Vec<AgentRow> {
    agents
        .iter()
        .map(|a| AgentRow {
            agent: a.display_name.to_string(),
            version: a.version.clone(),
            status: display_status(a),
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
    let agents = detect::detect_all_with(registry, cache, runner);
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
    let agents = detect::detect_all_with(registry, cache, runner);
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
            if a.status == "unknown"
                && let Some(ref err) = a.error
            {
                println!("  {}: {} — {}", a.display_name, a.binary_path, err);
            }
        }
    }
}

pub fn cli_list(json: bool) {
    let mut state = state::load();
    run_list(
        agent_registry::REGISTRY,
        &mut state.version_cache,
        &detect::RealVersionRunner,
        json,
    );
    state::save(&state);
}

pub fn cli_doctor(json: bool) {
    let mut state = state::load();
    run_doctor(
        agent_registry::REGISTRY,
        &mut state.version_cache,
        &detect::RealVersionRunner,
        json,
    );
    state::save(&state);
}

pub fn cli_install(agent: &str, dry_run: bool) {
    match agent_install::run_install(agent_registry::REGISTRY, agent, None, dry_run) {
        Outcome::Ok(msg) => println!("{msg}"),
        Outcome::Skipped(msg) => println!("skip: {msg}"),
        Outcome::Err(msg) => crate::ui::error(&msg),
    }
}

pub fn cli_update(agent: &str, dry_run: bool) {
    match agent_install::run_update(agent_registry::REGISTRY, agent, dry_run) {
        Outcome::Ok(msg) => println!("{msg}"),
        Outcome::Skipped(msg) => println!("skip: {msg}"),
        Outcome::Err(msg) => crate::ui::error(&msg),
    }
}

pub fn cli_uninstall(agent: &str, dry_run: bool) {
    match agent_install::run_uninstall(agent_registry::REGISTRY, agent, dry_run) {
        Outcome::Ok(msg) => println!("{msg}"),
        Outcome::Skipped(msg) => println!("skip: {msg}"),
        Outcome::Err(msg) => crate::ui::error(&msg),
    }
}

/// Env for a launched agent so its hooks register under a team and name
/// (`identity::touch_hook_session`). Blank values are dropped.
pub fn team_env(team: Option<&str>, name: Option<&str>) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if let Some(t) = team.map(str::trim).filter(|t| !t.is_empty()) {
        env.push((
            crate::messages::identity::TEAM_ENV.to_string(),
            t.to_string(),
        ));
    }
    if let Some(n) = name.map(str::trim).filter(|n| !n.is_empty()) {
        env.push(("AGENTFLARE_SESSION_NAME".to_string(), n.to_string()));
    }
    env
}

pub fn cli_launch(
    agent: &str,
    model: Option<&str>,
    mode: Option<&str>,
    args: &[String],
    team: Option<&str>,
    name: Option<&str>,
) {
    let env = team_env(team, name);
    match agent_launch::run_launch_env(
        agent_registry::REGISTRY,
        agent,
        model,
        mode,
        args,
        &env,
        false,
    ) {
        LaunchOutcome::Launched => {}
        LaunchOutcome::NotFound(msg) => crate::ui::error(&msg),
        LaunchOutcome::UnknownAgent(msg) => crate::ui::error(&format!("unknown agent: {msg}")),
        LaunchOutcome::Extension(msg) => crate::ui::error(&msg),
    }
}

/// `agentflare run <agent>` — launch through mise (so its tools are on PATH) with
/// wrangler-style `.dev.vars`[.<stage>] env vars injected. Reports what it
/// injects on stderr so it doesn't pollute the agent's stdout.
pub fn cli_run(
    agent: &str,
    stage: Option<&str>,
    model: Option<&str>,
    mode: Option<&str>,
    args: &[String],
    team: Option<&str>,
    name: Option<&str>,
) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let vault_env = crate::vault::vault_env(&cwd);
    let mut env: Vec<(String, String)> = vault_env.into_iter().collect();
    if let Some((path, vars)) = crate::dev_vars::load(&cwd, stage) {
        eprintln!(
            "agentflare run: injecting {} var(s) from {}",
            vars.len(),
            path.display()
        );
        env.extend(vars);
    } else if let Some(s) = stage {
        crate::ui::warning(&format!(
            "agentflare run: no .dev.vars.{s} or .dev.vars found"
        ));
    }
    env.extend(team_env(team, name));
    match agent_launch::run_launch_env(
        agent_registry::REGISTRY,
        agent,
        model,
        mode,
        args,
        &env,
        true,
    ) {
        LaunchOutcome::Launched => {}
        LaunchOutcome::NotFound(msg) => crate::ui::error(&msg),
        LaunchOutcome::UnknownAgent(msg) => crate::ui::error(&format!("unknown agent: {msg}")),
        LaunchOutcome::Extension(msg) => crate::ui::error(&msg),
    }
}

/// Headless variant of `cli_run`: run the agent non-interactively with `prompt`,
/// print its captured reply to stdout, and return a process exit code (0 on
/// success, 1 on any failure). The caller decides whether to `exit`.
pub fn cli_run_headless(agent: &str, prompt: &str, timeout: std::time::Duration) -> i32 {
    // `agentflare run --print` is a single one-off invocation, not dispatched
    // background work — pass `timeout` as both the hard cap and the idle
    // window so behavior matches the old flat wall-clock deadline exactly.
    match agent_launch::run_headless(
        agent_registry::REGISTRY,
        agent,
        prompt,
        timeout,
        timeout,
        &[],
        false,
    ) {
        agent_launch::HeadlessOutcome::Ok(reply) => {
            print!("{}", reply.text);
            0
        }
        agent_launch::HeadlessOutcome::UnknownAgent(msg)
        | agent_launch::HeadlessOutcome::NotHeadless(msg)
        | agent_launch::HeadlessOutcome::NotFound(msg)
        | agent_launch::HeadlessOutcome::Failed(msg) => {
            crate::ui::error(&msg);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_registry::{Agent, Tier};
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
        let _guard = agent_registry::detect::PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir =
            std::env::temp_dir().join(format!("agentflare-test-agents-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = std::env::var_os("PATH");
        unsafe {
            // SAFETY: PATH_LOCK mutex serializes all PATH mutations;
            // no other thread can read or write PATH concurrently.
            std::env::set_var("PATH", &dir)
        };
        f(&dir);
        match original {
            Some(p) => unsafe {
                // SAFETY: PATH_LOCK mutex serializes all PATH mutations.
                std::env::set_var("PATH", p)
            },
            None => unsafe {
                // SAFETY: PATH_LOCK mutex serializes all PATH mutations.
                std::env::remove_var("PATH")
            },
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
    fn team_env_sets_only_what_is_given() {
        assert!(team_env(None, None).is_empty());
        assert_eq!(
            team_env(Some("alpha"), Some("pm")),
            vec![
                ("AGENTFLARE_TEAM".to_string(), "alpha".to_string()),
                ("AGENTFLARE_SESSION_NAME".to_string(), "pm".to_string()),
            ]
        );
        assert_eq!(team_env(Some(" "), None), vec![]);
    }

    #[test]
    fn list_json_output_contains_expected_fields() {
        with_temp_path_dir(|dir| {
            std::fs::write(dir.join("testbin"), "").unwrap();
            let runner = FakeRunner {
                response: Ok("1.2.3".to_string()),
            };
            let mut cache = HashMap::new();

            let detected = detect::detect_all_with(&test_registry(), &mut cache, &runner);
            let output = ListOutput {
                agents: to_rows(&detected),
            };
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
            let detected = detect::detect_all_with(&registry, &mut cache, &runner);
            let output = ListOutput {
                agents: to_rows(&detected),
            };
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

        let detected = detect::detect_all_with(&empty_registry, &mut cache, &runner);
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
            let detected = detect::detect_all_with(&registry, &mut cache, &runner);
            let output = ListOutput {
                agents: to_rows(&detected),
            };
            let json = serde_json::to_string_pretty(&output).unwrap();
            assert!(json.contains("\"agent\": \"dr-agent\""));
            assert!(json.contains("\"version\": \"7.8.9\""));
        });
    }

    #[test]
    fn cli_run_headless_unknown_agent_returns_error_code() {
        let code = cli_run_headless("nope-not-an-agent", "hi", std::time::Duration::from_secs(1));
        assert_eq!(code, 1);
    }
}
