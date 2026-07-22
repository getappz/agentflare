use clap::{Args, Subcommand};
use skill::agents::AgentRegistry;
use skill::git::clone_repo;
use skill::manager::SkillManager;
use skill::skills::discover_skills;
use skill::source::parse_source;
use skill::types::{
    AgentConfig, AgentId, DiscoverOptions, InstallMode, InstallOptions, InstallScope,
};
use std::collections::HashSet;
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
    /// Run search-quality evaluation against the indexed skills and report
    /// Hit@1/Hit@3/MRR/nDCG. Fails with a non-zero exit when any metric
    /// drops below its configured floor.
    Eval,
    /// Export all skills to a JSON bundle file.
    Export {
        /// Output file path (default: skills-bundle.json)
        output: Option<String>,
    },
    /// Import skills from a JSON bundle file (with dedup).
    Import {
        /// Path to the JSON bundle file.
        path: String,
    },
    /// Push/pull skill bundles to/from a remote hub.
    Hub {
        #[command(subcommand)]
        action: HubAction,
    },
}

#[derive(Subcommand)]
pub enum HubAction {
    /// Pull skills from a remote hub URL and merge into local DB.
    Pull { url: String },
    /// Push local skills to a remote hub URL.
    Push { url: String },
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

/// Labeled query for `skill eval`. `relevance` is on a 0-3 scale:
/// 3 = perfect match (the skill is about exactly this), 2 = good match,
/// 1 = marginal, 0 = irrelevant.
struct EvalQuery {
    query: &'static str,
    expected: &'static str,
    relevance: u32,
}

const EVAL_QUERIES: &[EvalQuery] = &[
    EvalQuery {
        query: "what's running right now",
        expected: "live",
        relevance: 3,
    },
    EvalQuery {
        query: "are my agents stuck",
        expected: "live",
        relevance: 3,
    },
    EvalQuery {
        query: "check on background sessions",
        expected: "live",
        relevance: 3,
    },
    EvalQuery {
        query: "how much did I spend on tokens this week",
        expected: "cv-usage",
        relevance: 3,
    },
    EvalQuery {
        query: "usage statistics",
        expected: "cv-usage",
        relevance: 3,
    },
    EvalQuery {
        query: "session count report",
        expected: "cv-usage",
        relevance: 2,
    },
    EvalQuery {
        query: "my disk is full",
        expected: "win-cleanup",
        relevance: 3,
    },
    EvalQuery {
        query: "free up space on windows",
        expected: "win-cleanup",
        relevance: 3,
    },
    EvalQuery {
        query: "clean temp files",
        expected: "win-cleanup",
        relevance: 3,
    },
    EvalQuery {
        query: "review my diff for bugs",
        expected: "code-review",
        relevance: 3,
    },
    EvalQuery {
        query: "check this code for correctness",
        expected: "code-review",
        relevance: 3,
    },
    EvalQuery {
        query: "find efficiency cleanups",
        expected: "code-review",
        relevance: 2,
    },
    EvalQuery {
        query: "research this topic with cited sources",
        expected: "deep-research",
        relevance: 3,
    },
    EvalQuery {
        query: "fan out web searches and verify claims",
        expected: "deep-research",
        relevance: 3,
    },
    EvalQuery {
        query: "write me a fact checked report",
        expected: "deep-research",
        relevance: 3,
    },
    EvalQuery {
        query: "this skill is too verbose",
        expected: "short-skill",
        relevance: 3,
    },
    EvalQuery {
        query: "compress a bloated skill",
        expected: "short-skill",
        relevance: 3,
    },
    EvalQuery {
        query: "make a shorthand version of a skill",
        expected: "short-skill",
        relevance: 3,
    },
    EvalQuery {
        query: "skill is token heavy",
        expected: "short-skill",
        relevance: 3,
    },
    EvalQuery {
        query: "what needs my attention",
        expected: "live",
        relevance: 3,
    },
    EvalQuery {
        query: "system slow disk space",
        expected: "win-cleanup",
        relevance: 2,
    },
    EvalQuery {
        query: "token spend this month",
        expected: "cv-usage",
        relevance: 3,
    },
    EvalQuery {
        query: "debug this pull request",
        expected: "code-review",
        relevance: 2,
    },
    EvalQuery {
        query: "synthesize research findings",
        expected: "deep-research",
        relevance: 3,
    },
    EvalQuery {
        query: "make a shorter skill",
        expected: "short-skill",
        relevance: 3,
    },
];

struct EvalReport {
    hit_at_1: f64,
    hit_at_3: f64,
    mrr: f64,
    ndcg: f64,
    total: usize,
    passes: u32,
}

fn run_eval() -> Result<EvalReport, String> {
    let db_path = crate::paths::skills_db_path();
    let mut registry =
        skill_registry::Registry::open_default(&db_path).map_err(|e| e.to_string())?;
    registry.ensure_fresh().map_err(|e| e.to_string())?;

    let mut hit1 = 0usize;
    let mut hit3 = 0usize;
    let mut reciprocal_ranks = Vec::with_capacity(EVAL_QUERIES.len());
    let mut dcg_scores = Vec::with_capacity(EVAL_QUERIES.len());
    use skill_registry::MatchMode;

    for eq in EVAL_QUERIES {
        let mut r = registry
            .search(eq.query, 3, MatchMode::All)
            .unwrap_or_default();
        if r.is_empty() {
            r = registry
                .search(eq.query, 3, MatchMode::Any)
                .unwrap_or_default();
        }

        let ideal_dcg = (eq.relevance as f64)
            + if eq.relevance > 0 {
                eq.relevance as f64 / (2f64).log2()
            } else {
                0.0
            };

        let dcg = r.first().map_or(0.0, |h| {
            if h.name == eq.expected {
                eq.relevance as f64
            } else {
                0.0
            }
        });

        dcg_scores.push((dcg, ideal_dcg));

        if r.first().is_some_and(|h| h.name == eq.expected) {
            hit1 += 1;
            hit3 += 1;
            reciprocal_ranks.push(1.0);
        } else if r.iter().any(|h| h.name == eq.expected) {
            hit3 += 1;
            let pos = r.iter().position(|h| h.name == eq.expected).unwrap_or(2) + 1;
            reciprocal_ranks.push(1.0 / pos as f64);
        } else {
            reciprocal_ranks.push(0.0);
        }
    }

    let total = EVAL_QUERIES.len();
    let hit_at_1 = hit1 as f64 / total as f64;
    let hit_at_3 = hit3 as f64 / total as f64;
    let mrr = reciprocal_ranks.iter().sum::<f64>() / total as f64;
    let ndcg = dcg_scores
        .iter()
        .map(|(d, i)| if *i > 0.0 { d / i } else { 0.0 })
        .sum::<f64>()
        / total as f64;

    let mut passes = 0u32;
    if hit_at_1 >= 0.70 {
        passes += 1;
    }
    if hit_at_3 >= 0.85 {
        passes += 1;
    }
    if mrr >= 0.75 {
        passes += 1;
    }
    if ndcg >= 0.80 {
        passes += 1;
    }

    Ok(EvalReport {
        hit_at_1,
        hit_at_3,
        mrr,
        ndcg,
        total,
        passes,
    })
}

fn print_eval(report: &EvalReport) {
    println!("━━━ skill eval ━━━");
    println!("  queries:  {}", report.total);
    println!(
        "  Hit@1:    {:.3}  {}",
        report.hit_at_1,
        if report.hit_at_1 >= 0.70 {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "  Hit@3:    {:.3}  {}",
        report.hit_at_3,
        if report.hit_at_3 >= 0.85 {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "  MRR:      {:.3}  {}",
        report.mrr,
        if report.mrr >= 0.75 { "✓" } else { "✗" }
    );
    println!(
        "  nDCG:     {:.3}  {}",
        report.ndcg,
        if report.ndcg >= 0.80 { "✓" } else { "✗" }
    );
    let verdict = if report.passes == 4 { "PASS" } else { "FAIL" };
    println!("  verdict:  {verdict} ({}/{})", report.passes, 4);
}

fn run_export(output: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let db_path = crate::paths::skills_db_path();
    let conn = skill_registry::db::open_db(&db_path)?;
    let pairs = skill_registry::search::list_all_name_source_pairs(&conn)?;
    let mut entries = Vec::new();
    for (skill_name, source) in &pairs {
        let qualified = format!("{source}:{skill_name}");
        let skill = skill_registry::load(&conn, &qualified, false)?;
        entries.push(skill_registry::sources::SkillEntry {
            name: skill_name.clone(),
            source: source.clone(),
            path: PathBuf::new(),
            description: skill.description.clone(),
            body: skill.body.clone(),
            neg_text: String::new(),
            tags: String::new(),
            est_tokens: skill.body.len() as i64 / 4,
            mtime: 0,
            bandit_alpha: 1.0,
            bandit_beta: 1.0,
            shadow_path: None,
        });
    }
    let bundle = skill_registry::SkillBundle::new(&entries);
    let path = output.unwrap_or("skills-bundle.json");
    std::fs::write(path, bundle.to_json()?)?;
    println!("exported {} skills to {path}", entries.len());
    Ok(())
}

fn run_import(path: &str) -> Result<usize, Box<dyn std::error::Error>> {
    let json = std::fs::read_to_string(path)?;
    let mut bundle = skill_registry::SkillBundle::from_json(&json)?;
    let deduped = bundle.dedup();
    if deduped > 0 {
        eprintln!("note: removed {deduped} duplicate entries during import");
    }
    let db_path = crate::paths::skills_db_path();
    let mut conn = skill_registry::db::open_db(&db_path)?;
    let entries = bundle.to_entries(Path::new("import"));
    skill_registry::db::rebuild(&mut conn, &entries)?;
    Ok(entries.len())
}

fn run_hub(action: HubAction) -> Result<String, Box<dyn std::error::Error>> {
    match action {
        HubAction::Pull { url } => {
            let bundle = skill_registry::hub::pull_bundle(&url)?;
            let db_path = crate::paths::skills_db_path();
            let mut conn = skill_registry::db::open_db(&db_path)?;
            // Read existing skills from scan sources first, then merge
            // hub entries on top.
            let home = dirs::data_local_dir().unwrap_or_else(std::env::temp_dir);
            let cwd = std::env::current_dir().ok();
            let cwd = cwd.as_deref().unwrap_or(Path::new("."));
            let sources =
                skill_registry::sources::default_sources(&home, cwd, &["claude-code".to_string()]);
            let scan = skill_registry::sources::scan_sources(&sources);
            let local_count = scan.entries.len();
            let mut all = scan.entries;
            let local_keys: HashSet<(String, String)> = all
                .iter()
                .map(|e| (e.name.clone(), e.source.clone()))
                .collect();
            for e in bundle.to_entries(Path::new("hub")) {
                if !local_keys.contains(&(e.name.clone(), e.source.clone())) {
                    all.push(e);
                }
            }
            skill_registry::db::rebuild(&mut conn, &all)?;
            let net_new = all.len() - local_count;
            Ok(format!(
                "pulled {net_new} skills from hub, total {}",
                all.len()
            ))
        }
        HubAction::Push { url } => {
            let db_path = crate::paths::skills_db_path();
            let conn = skill_registry::db::open_db(&db_path)?;
            let pairs = skill_registry::search::list_all_name_source_pairs(&conn)?;
            let mut entries = Vec::new();
            for (skill_name, source) in &pairs {
                let qualified = format!("{source}:{skill_name}");
                let skill = skill_registry::load(&conn, &qualified, false)?;
                entries.push(skill_registry::sources::SkillEntry {
                    name: skill_name.clone(),
                    source: source.clone(),
                    path: PathBuf::new(),
                    description: skill.description.clone(),
                    body: skill.body.clone(),
                    neg_text: String::new(),
                    tags: String::new(),
                    est_tokens: skill.body.len() as i64 / 4,
                    mtime: 0,
                    bandit_alpha: 1.0,
                    bandit_beta: 1.0,
                    shadow_path: None,
                });
            }
            let bundle = skill_registry::SkillBundle::new(&entries);
            skill_registry::hub::push_bundle(&url, &bundle)?;
            Ok(format!("pushed {} skills to {url}", entries.len()))
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
            SkillAction::Export { output } => {
                if let Err(e) = run_export(output.as_deref()) {
                    eprintln!("export error: {e}");
                    std::process::exit(1);
                }
            }
            SkillAction::Import { path } => match run_import(&path) {
                Err(e) => {
                    eprintln!("import error: {e}");
                    std::process::exit(1);
                }
                Ok(count) => println!("imported {count} skills"),
            },
            SkillAction::Hub { action } => match run_hub(action) {
                Err(e) => {
                    eprintln!("hub error: {e}");
                    std::process::exit(1);
                }
                Ok(msg) => println!("{msg}"),
            },
            SkillAction::Eval => match run_eval() {
                Ok(report) => {
                    print_eval(&report);
                    if report.passes < 4 {
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("eval error: {e}");
                    std::process::exit(1);
                }
            },
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
