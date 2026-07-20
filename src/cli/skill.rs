use std::path::{Path, PathBuf};
use clap::{Args, Subcommand};
use skill::agents::AgentRegistry;
use skill::git::clone_repo;
use skill::manager::SkillManager;
use skill::skills::discover_skills;
use skill::source::parse_source;
use skill::types::{
    AgentConfig, AgentId, DiscoverOptions, InstallMode, InstallOptions, InstallScope,
};

#[derive(Subcommand)]
pub enum SkillAction {
    Search { query: String },
    Install { name: String },
    List,
    Remove { name: String },
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

fn agentflare_config(home: &PathBuf) -> AgentConfig {
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
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
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
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".agentflare")
        .join("registries.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|b| serde_json::from_str(&b).ok())
        .unwrap_or_else(|| vec!["gh:getappz/skill-registry".into()])
}

fn save_registries(registries: &[String]) {
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
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
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(AGENTFLARE_SKILLS)
}

async fn try_install_from_source(
    manager: &SkillManager,
    url: &str,
    name: &str,
) -> Result<bool, skill::error::SkillError> {
    let parsed = parse_source(url);
    if parsed.source_type == skill::types::SourceType::Local {
        let lp = parsed.local_path.as_deref().unwrap_or_else(|| Path::new("."));
        let entries = discover_skills(lp, None, &DiscoverOptions::default()).await?;
        if let Some(skill) = entries.into_iter().find(|s| s.name == name) {
            manager.install_skill(&skill, &AgentId::new("agentflare"), &install_opts()).await?;
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
        manager.install_skill(&skill, &AgentId::new("agentflare"), &install_opts()).await?;
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
        cwd: Some(dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"))),
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
