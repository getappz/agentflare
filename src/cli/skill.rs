use clap::{Args, Subcommand};
use skill::agents::AgentRegistry;
use skill::git::clone_repo;
use skill::manager::SkillManager;
use skill::skills::discover_skills;
use skill::source::parse_source;
use skill::types::{
    AgentConfig, AgentId, DiscoverOptions, InstallMode, InstallOptions, InstallScope,
};
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum SkillAction {
    Search {
        query: String,
    },
    Install {
        name: String,
    },
    List,
    Remove {
        name: String,
    },
    Registry {
        #[command(subcommand)]
        action: RegistryAction,
    },
}

#[derive(Subcommand)]
pub enum RegistryAction {
    Add { url: String },
    Remove { url: String },
    List,
}

#[derive(Args)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub action: SkillAction,
}

const AGENTFLARE_SKILLS: &str = ".agentflare/skills";

fn agentflare_config(home: &Path) -> AgentConfig {
    AgentConfig {
        name: AgentId::new("agentflare"),
        display_name: "Agentflare".into(),
        skills_dir: AGENTFLARE_SKILLS.into(),
        global_skills_dir: Some(home.join(AGENTFLARE_SKILLS)),
        detect_paths: vec![],
        show_in_universal_list: false,
    }
}

fn build_manager() -> SkillManager {
    let home = crate::paths::home();
    let mut registry = AgentRegistry::with_defaults();
    registry.register(agentflare_config(&home));
    SkillManager::builder().agents(registry).build()
}

fn install_opts() -> InstallOptions {
    InstallOptions {
        scope: InstallScope::Global,
        mode: InstallMode::Copy,
        ..Default::default()
    }
}

fn load_registries() -> Vec<String> {
    let path = crate::paths::home()
        .join(".agentflare")
        .join("registries.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|b| serde_json::from_str(&b).ok())
        .unwrap_or_else(|| vec!["gh:getappz/skill-registry".into()])
}

fn save_registries(registries: &[String]) {
    let path = crate::paths::home()
        .join(".agentflare")
        .join("registries.json");
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if let Ok(b) = serde_json::to_string_pretty(registries) {
        let _ = std::fs::write(&path, b);
    }
}

fn home_skills() -> PathBuf {
    crate::paths::home().join(AGENTFLARE_SKILLS)
}

async fn try_install_from_source(
    manager: &SkillManager,
    url: &str,
    name: &str,
) -> Result<bool, skill::error::SkillError> {
    let parsed = parse_source(url);
    if parsed.source_type == skill::types::SourceType::Local {
        let lp = parsed
            .local_path
            .as_deref()
            .unwrap_or_else(|| Path::new("."));
        let entries = discover_skills(lp, None, &DiscoverOptions::default()).await?;
        if let Some(skill) = entries.into_iter().find(|s| s.name == name) {
            manager
                .install_skill(&skill, &AgentId::new("agentflare"), &install_opts())
                .await?;
            return Ok(true);
        }
        return Ok(false);
    }

    let repo_url = parsed.url.as_str();
    let temp = match clone_repo(repo_url, parsed.git_ref.as_deref()).await {
        Ok(d) => d,
        Err(_) => return Ok(false),
    };
    let entries = match parsed.subpath.as_ref() {
        Some(sub) => discover_skills(temp.path(), Some(sub), &DiscoverOptions::default()).await?,
        None => discover_skills(temp.path(), None, &DiscoverOptions::default()).await?,
    };
    if let Some(skill) = entries.into_iter().find(|s| s.name == name) {
        manager
            .install_skill(&skill, &AgentId::new("agentflare"), &install_opts())
            .await?;
        return Ok(true);
    }
    Ok(false)
}

async fn run_install(name: &str) -> Result<(), skill::error::SkillError> {
    if home_skills().join(name).join("SKILL.md").exists() {
        println!("'{name}' already installed");
        return Ok(());
    }

    let manager = build_manager();
    let sources = load_registries();

    for url in &sources {
        if try_install_from_source(&manager, url, name).await? {
            println!("✓ installed '{name}'");
            return Ok(());
        }
    }

    eprintln!("skill '{name}' not found in any registry");
    std::process::exit(1);
}

async fn run_list() -> Result<(), skill::error::SkillError> {
    let manager = build_manager();
    let opts = skill::types::ListOptions {
        scope: Some(InstallScope::Global),
        ..Default::default()
    };
    let installed = manager.list_installed(&opts).await?;
    if installed.is_empty() {
        println!("no skills installed");
        return Ok(());
    }
    for s in &installed {
        println!("  {} — {}", s.name, s.description);
    }
    Ok(())
}

async fn run_remove(name: &str) -> Result<(), skill::error::SkillError> {
    let manager = build_manager();
    let opts = skill::types::RemoveOptions {
        scope: InstallScope::Global,
        cwd: Some(crate::paths::home()),
        ..Default::default()
    };
    manager.remove_skills(&[name.to_string()], &opts).await?;
    println!("✓ removed '{name}'");
    Ok(())
}

fn run_search(query: &str) {
    let skills_dir = home_skills();
    if skills_dir.is_dir() {
        let matched: Vec<_> = std::fs::read_dir(&skills_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                let name = e.file_name();
                let n = name.to_string_lossy();
                n.to_lowercase().contains(&query.to_lowercase())
            })
            .collect();
        if !matched.is_empty() {
            println!("── installed ──");
            for e in &matched {
                println!("  {}", e.file_name().to_string_lossy());
            }
        }
    }
    println!("── registries ──");
    for r in load_registries() {
        println!("  {r}");
    }
    println!("use `agentflare skill install <name>` to install");
}

fn run_registry(action: RegistryAction) {
    let mut reg = load_registries();
    match action {
        RegistryAction::Add { url } => {
            let url = url.trim().to_string();
            if reg.contains(&url) {
                println!("already configured: {url}");
                return;
            }
            reg.push(url.clone());
            save_registries(&reg);
            println!("added: {url}");
        }
        RegistryAction::Remove { url } => {
            let n = reg.len();
            reg.retain(|r| r != &url);
            if reg.len() < n {
                save_registries(&reg);
                println!("removed: {url}");
            } else {
                eprintln!("registry not found: {url}");
                std::process::exit(1);
            }
        }
        RegistryAction::List => {
            if reg.is_empty() {
                println!("no registries configured");
                return;
            }
            for r in &reg {
                println!("  {r}");
            }
        }
    }
}

impl SkillArgs {
    pub fn run(self) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        match self.action {
            SkillAction::Search { query } => run_search(&query),
            SkillAction::Install { name } => {
                if let Err(e) = rt.block_on(run_install(&name)) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
            SkillAction::List => {
                if let Err(e) = rt.block_on(run_list()) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
            SkillAction::Remove { name } => {
                if let Err(e) = rt.block_on(run_remove(&name)) {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
            SkillAction::Registry { action } => run_registry(action),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Runtime::new().unwrap().block_on(f)
    }

    #[test]
    fn registry_add_list_remove_roundtrip() {
        crate::paths::test_support::with_temp_home(|| {
            assert_eq!(
                load_registries(),
                vec!["gh:getappz/skill-registry".to_string()]
            );

            run_registry(RegistryAction::Add {
                url: "https://github.com/example/skills".into(),
            });
            let regs = load_registries();
            assert!(regs.contains(&"https://github.com/example/skills".to_string()));

            run_registry(RegistryAction::Remove {
                url: "https://github.com/example/skills".into(),
            });
            let regs = load_registries();
            assert!(!regs.contains(&"https://github.com/example/skills".to_string()));
        });
    }

    #[test]
    fn registry_add_is_idempotent() {
        crate::paths::test_support::with_temp_home(|| {
            let url = "https://github.com/example/skills".to_string();
            run_registry(RegistryAction::Add { url: url.clone() });
            run_registry(RegistryAction::Add { url: url.clone() });
            let regs = load_registries();
            assert_eq!(regs.iter().filter(|r| **r == url).count(), 1);
        });
    }

    fn write_fixture_skill(dir: &Path, name: &str, description: &str) {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\nBody.\n"),
        )
        .unwrap();
    }

    #[test]
    fn install_from_local_source_copies_skill() {
        crate::paths::test_support::with_temp_home(|| {
            let src = tempfile::tempdir().unwrap();
            write_fixture_skill(
                src.path(),
                "my-test-skill",
                "A test skill for install verification",
            );

            let manager = build_manager();
            let url = src.path().to_string_lossy().into_owned();
            let found = block_on(try_install_from_source(&manager, &url, "my-test-skill")).unwrap();
            assert!(
                found,
                "expected try_install_from_source to find and install the skill"
            );

            let installed = home_skills().join("my-test-skill").join("SKILL.md");
            assert!(
                installed.exists(),
                "expected {installed:?} to exist after install"
            );
        });
    }

    #[test]
    fn install_from_local_source_returns_false_when_name_not_found() {
        crate::paths::test_support::with_temp_home(|| {
            let src = tempfile::tempdir().unwrap();
            write_fixture_skill(src.path(), "other-skill", "Unrelated skill");

            let manager = build_manager();
            let url = src.path().to_string_lossy().into_owned();
            let found = block_on(try_install_from_source(&manager, &url, "my-test-skill")).unwrap();
            assert!(!found);
        });
    }

    #[test]
    fn run_install_end_to_end_via_registry() {
        crate::paths::test_support::with_temp_home(|| {
            let src = tempfile::tempdir().unwrap();
            write_fixture_skill(
                src.path(),
                "my-test-skill",
                "A test skill for install verification",
            );
            save_registries(&[src.path().to_string_lossy().into_owned()]);

            block_on(run_install("my-test-skill")).unwrap();

            let installed = home_skills().join("my-test-skill").join("SKILL.md");
            assert!(
                installed.exists(),
                "expected {installed:?} to exist after install"
            );
        });
    }
}
