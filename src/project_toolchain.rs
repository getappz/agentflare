use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectProfile {
    pub languages: Vec<String>,
    pub frameworks: Vec<String>,
    pub package_managers: Vec<String>,
    pub build_tools: Vec<String>,
    pub is_monorepo: bool,
    pub confidence: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RankedSkill {
    pub name: String,
    pub source: String,
    pub description: String,
    pub tags: Vec<String>,
    pub score: f64,
    pub match_reasons: Vec<String>,
    pub est_tokens: i64,
}

fn detect_package_manager(cwd: &Path) -> String {
    if cwd.join("pnpm-lock.yaml").exists() {
        "pnpm".to_string()
    } else if cwd.join("yarn.lock").exists() {
        "yarn".to_string()
    } else if cwd.join("bun.lockb").exists() {
        "bun".to_string()
    } else {
        "npm".to_string()
    }
}

fn parse_cargo_toml(cwd: &Path, profile: &mut ProjectProfile) {
    let cargo_toml = cwd.join("Cargo.toml");
    if let Ok(content) = std::fs::read_to_string(&cargo_toml)
        && let Ok(value) = content.parse::<toml::Value>()
    {
        if let Some(deps) = value.get("dependencies").and_then(|d| d.as_table()) {
            for (name, _) in deps {
                match name.as_str() {
                    "actix-web" | "actix" => profile.frameworks.push("actix".to_string()),
                    "axum" => profile.frameworks.push("axum".to_string()),
                    "rocket" => profile.frameworks.push("rocket".to_string()),
                    "warp" => profile.frameworks.push("warp".to_string()),
                    "tokio" => profile.frameworks.push("tokio".to_string()),
                    "async-std" => profile.frameworks.push("async-std".to_string()),
                    "serde" => profile.frameworks.push("serde".to_string()),
                    "diesel" => profile.frameworks.push("diesel".to_string()),
                    "sqlx" => profile.frameworks.push("sqlx".to_string()),
                    "tauri" => profile.frameworks.push("tauri".to_string()),
                    "leptos" => profile.frameworks.push("leptos".to_string()),
                    "dioxus" => profile.frameworks.push("dioxus".to_string()),
                    "yew" => profile.frameworks.push("yew".to_string()),
                    _ => {}
                }
            }
        }
        if value.get("workspace").is_some() {
            profile.is_monorepo = true;
        }
    }
}

fn parse_package_json(cwd: &Path, profile: &mut ProjectProfile) {
    let package_json = cwd.join("package.json");
    if let Ok(content) = std::fs::read_to_string(&package_json)
        && let Ok(value) = content.parse::<serde_json::Value>()
    {
        let deps = value
            .get("dependencies")
            .and_then(|d| d.as_object())
            .into_iter()
            .flat_map(|o| o.keys())
            .chain(
                value
                    .get("devDependencies")
                    .and_then(|d| d.as_object())
                    .into_iter()
                    .flat_map(|o| o.keys()),
            )
            .collect::<HashSet<_>>();

        for dep in deps {
            match dep.as_str() {
                "react" | "react-dom" => profile.frameworks.push("react".to_string()),
                "next" => profile.frameworks.push("nextjs".to_string()),
                "vue" => profile.frameworks.push("vue".to_string()),
                "nuxt" => profile.frameworks.push("nuxt".to_string()),
                "svelte" => profile.frameworks.push("svelte".to_string()),
                "express" => profile.frameworks.push("express".to_string()),
                "fastify" => profile.frameworks.push("fastify".to_string()),
                "nestjs" | "@nestjs/core" => profile.frameworks.push("nestjs".to_string()),
                "vite" => profile.build_tools.push("vite".to_string()),
                "webpack" => profile.build_tools.push("webpack".to_string()),
                "turbo" | "turbo-repo" => profile.build_tools.push("turborepo".to_string()),
                "nx" => profile.build_tools.push("nx".to_string()),
                "jest" | "vitest" => profile.frameworks.push("testing".to_string()),
                "typescript" if !profile.languages.contains(&"typescript".to_string()) => {
                    profile.languages.push("typescript".to_string());
                }
                _ => {}
            }
        }

        if let Some(workspaces) = value.get("workspaces").and_then(|w| w.as_array())
            && !workspaces.is_empty()
        {
            profile.is_monorepo = true;
        }

        profile.package_managers.push(detect_package_manager(cwd));
    }
}

fn parse_pyproject_toml(cwd: &Path, profile: &mut ProjectProfile) {
    let pyproject = cwd.join("pyproject.toml");
    if let Ok(content) = std::fs::read_to_string(&pyproject)
        && let Ok(value) = content.parse::<toml::Value>()
    {
        let deps = value
            .get("project")
            .and_then(|p| p.get("dependencies"))
            .and_then(|d| d.as_array())
            .into_iter()
            .flat_map(|a| a.iter().filter_map(|v| v.as_str()))
            .chain(
                value
                    .get("tool")
                    .and_then(|t| t.get("poetry"))
                    .and_then(|p| p.get("dependencies"))
                    .and_then(|d| d.as_table())
                    .into_iter()
                    .flat_map(|t| t.keys().map(String::as_str)),
            )
            .chain(
                value
                    .get("tool")
                    .and_then(|t| t.get("pdm"))
                    .and_then(|p| p.get("dependencies"))
                    .and_then(|d| d.as_table())
                    .into_iter()
                    .flat_map(|t| t.keys().map(String::as_str)),
            )
            .collect::<HashSet<_>>();

        for dep in deps {
            match dep.to_lowercase().as_str() {
                "django" => profile.frameworks.push("django".to_string()),
                "fastapi" => profile.frameworks.push("fastapi".to_string()),
                "flask" => profile.frameworks.push("flask".to_string()),
                "pydantic" => profile.frameworks.push("pydantic".to_string()),
                "sqlalchemy" => profile.frameworks.push("sqlalchemy".to_string()),
                "httpx" => profile.frameworks.push("httpx".to_string()),
                "typer" => profile.frameworks.push("typer".to_string()),
                "pytest" => profile.frameworks.push("testing".to_string()),
                "ruff" => profile.build_tools.push("ruff".to_string()),
                "maturin" => profile.build_tools.push("maturin".to_string()),
                _ => {}
            }
        }

        if value.get("tool").and_then(|t| t.get("poetry")).is_some() {
            profile.package_managers.push("poetry".to_string());
        } else if value.get("tool").and_then(|t| t.get("pdm")).is_some() {
            profile.package_managers.push("pdm".to_string());
        } else if value.get("tool").and_then(|t| t.get("hatch")).is_some() {
            profile.package_managers.push("hatch".to_string());
        } else if value.get("build-system").is_some() {
            profile.package_managers.push("pip".to_string());
        }
    }
}

fn parse_requirements_txt(cwd: &Path, profile: &mut ProjectProfile) {
    let req_txt = cwd.join("requirements.txt");
    if let Ok(content) = std::fs::read_to_string(&req_txt) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let dep = line
                .split(&['=', '>', '<', '!', '~'][..])
                .next()
                .unwrap_or(line);
            match dep.to_lowercase().as_str() {
                "django" => profile.frameworks.push("django".to_string()),
                "fastapi" => profile.frameworks.push("fastapi".to_string()),
                "flask" => profile.frameworks.push("flask".to_string()),
                "pydantic" => profile.frameworks.push("pydantic".to_string()),
                "sqlalchemy" => profile.frameworks.push("sqlalchemy".to_string()),
                _ => {}
            }
        }
        if profile.package_managers.is_empty() {
            profile.package_managers.push("pip".to_string());
        }
    }
}

fn parse_go_mod(cwd: &Path, profile: &mut ProjectProfile) {
    let go_mod = cwd.join("go.mod");
    if let Ok(content) = std::fs::read_to_string(&go_mod) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("require") || (line.contains('/') && !line.starts_with("module")) {
                if line.contains("gin-gonic/gin") {
                    profile.frameworks.push("gin".to_string());
                } else if line.contains("labstack/echo") {
                    profile.frameworks.push("echo".to_string());
                } else if line.contains("gofiber/fiber") {
                    profile.frameworks.push("fiber".to_string());
                } else if line.contains("gorilla/mux") {
                    profile.frameworks.push("gorilla".to_string());
                }
            }
        }
        profile.package_managers.push("go modules".to_string());
    }
}

pub fn detect_project_type(cwd: &Path) -> ProjectProfile {
    let mut profile = ProjectProfile::default();

    if cwd.join("Cargo.toml").exists() {
        profile.languages.push("rust".to_string());
        profile.package_managers.push("cargo".to_string());
        profile.build_tools.push("cargo".to_string());
        parse_cargo_toml(cwd, &mut profile);
        profile.confidence.insert("rust".to_string(), 1.0);
    }

    if cwd.join("package.json").exists() {
        if !profile.languages.contains(&"javascript".to_string()) {
            profile.languages.push("javascript".to_string());
        }
        if cwd.join("tsconfig.json").exists()
            && !profile.languages.contains(&"typescript".to_string())
        {
            profile.languages.push("typescript".to_string());
        }
        parse_package_json(cwd, &mut profile);
        profile.confidence.insert("javascript".to_string(), 1.0);
    }

    if cwd.join("pyproject.toml").exists() {
        profile.languages.push("python".to_string());
        parse_pyproject_toml(cwd, &mut profile);
        profile.confidence.insert("python".to_string(), 1.0);
    } else if cwd.join("requirements.txt").exists() {
        profile.languages.push("python".to_string());
        parse_requirements_txt(cwd, &mut profile);
        profile.confidence.insert("python".to_string(), 0.9);
    }

    if cwd.join("go.mod").exists() {
        profile.languages.push("go".to_string());
        parse_go_mod(cwd, &mut profile);
        profile.confidence.insert("go".to_string(), 1.0);
    }

    if cwd.join("pom.xml").exists() {
        profile.languages.push("java".to_string());
        profile.build_tools.push("maven".to_string());
        profile.confidence.insert("java".to_string(), 1.0);
    }

    if cwd.join("build.gradle").exists() || cwd.join("build.gradle.kts").exists() {
        if cwd.join("build.gradle.kts").exists() {
            profile.languages.push("kotlin".to_string());
        } else {
            profile.languages.push("java".to_string());
        }
        profile.build_tools.push("gradle".to_string());
        profile.confidence.insert("jvm".to_string(), 1.0);
    }

    let csproj_files: Vec<_> = std::fs::read_dir(cwd)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "csproj"))
        .collect();
    if !csproj_files.is_empty() {
        profile.languages.push("csharp".to_string());
        profile.build_tools.push("dotnet".to_string());
        profile.confidence.insert("csharp".to_string(), 1.0);
    }

    if cwd.join("CMakeLists.txt").exists() {
        profile.languages.push("cpp".to_string());
        profile.build_tools.push("cmake".to_string());
        profile.confidence.insert("cpp".to_string(), 1.0);
    } else if cwd.join("meson.build").exists() {
        profile.languages.push("cpp".to_string());
        profile.build_tools.push("meson".to_string());
        profile.confidence.insert("cpp".to_string(), 1.0);
    }

    profile.languages.sort();
    profile.languages.dedup();
    profile.frameworks.sort();
    profile.frameworks.dedup();
    profile.package_managers.sort();
    profile.package_managers.dedup();
    profile.build_tools.sort();
    profile.build_tools.dedup();

    profile
}

pub fn rank_skills_for_project(
    profile: &ProjectProfile,
    skills: Vec<skill_registry::search::SkillHit>,
) -> Vec<RankedSkill> {
    let mut ranked: Vec<RankedSkill> = skills
        .into_iter()
        .map(|hit| {
            let mut score = 0.0;
            let mut reasons = Vec::new();

            let skill_tags: HashSet<String> = hit
                .description
                .to_lowercase()
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();

            for lang in &profile.languages {
                if skill_tags.contains(&lang.to_lowercase()) {
                    score += 10.0;
                    reasons.push(format!("language:{}", lang));
                }
            }

            for fw in &profile.frameworks {
                if skill_tags.contains(&fw.to_lowercase()) {
                    score += 15.0;
                    reasons.push(format!("framework:{}", fw));
                }
            }

            for pm in &profile.package_managers {
                if skill_tags.contains(&pm.to_lowercase()) {
                    score += 5.0;
                    reasons.push(format!("pkg_mgr:{}", pm));
                }
            }

            for bt in &profile.build_tools {
                if skill_tags.contains(&bt.to_lowercase()) {
                    score += 5.0;
                    reasons.push(format!("build:{}", bt));
                }
            }

            if profile.languages.contains(&"rust".to_string()) && skill_tags.contains("systems") {
                score += 3.0;
                reasons.push("category:systems".to_string());
            }
            if (profile.languages.contains(&"javascript".to_string())
                || profile.languages.contains(&"typescript".to_string()))
                && skill_tags.contains("web")
            {
                score += 3.0;
                reasons.push("category:web".to_string());
            }
            if profile.languages.contains(&"python".to_string()) && skill_tags.contains("data") {
                score += 3.0;
                reasons.push("category:data".to_string());
            }

            RankedSkill {
                name: hit.name,
                source: hit.source,
                description: hit.description,
                tags: skill_tags.into_iter().collect(),
                score,
                match_reasons: reasons,
                est_tokens: hit.est_tokens,
            }
        })
        .collect();

    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detect_rust_project() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"test\"\n[dependencies]\naxum = \"0.7\"\n",
        )
        .unwrap();

        let profile = detect_project_type(dir.path());
        assert!(profile.languages.contains(&"rust".to_string()));
        assert!(profile.frameworks.contains(&"axum".to_string()));
        assert!(profile.package_managers.contains(&"cargo".to_string()));
    }

    #[test]
    fn detect_node_project() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name": "test", "dependencies": {"react": "^18", "next": "^14"}}"#,
        )
        .unwrap();
        fs::write(dir.path().join("package-lock.json"), "{}").unwrap();

        let profile = detect_project_type(dir.path());
        assert!(profile.languages.contains(&"javascript".to_string()));
        assert!(profile.frameworks.contains(&"react".to_string()));
        assert!(profile.frameworks.contains(&"nextjs".to_string()));
        assert!(profile.package_managers.contains(&"npm".to_string()));
    }

    #[test]
    fn detect_python_project() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            r#"[project]
name = "test"
dependencies = ["fastapi", "pydantic"]"#,
        )
        .unwrap();

        let profile = detect_project_type(dir.path());
        assert!(profile.languages.contains(&"python".to_string()));
        assert!(profile.frameworks.contains(&"fastapi".to_string()));
        assert!(profile.frameworks.contains(&"pydantic".to_string()));
    }

    #[test]
    fn detect_go_project() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module test\n\nrequire github.com/gin-gonic/gin v1.9.0\n",
        )
        .unwrap();

        let profile = detect_project_type(dir.path());
        assert!(profile.languages.contains(&"go".to_string()));
        assert!(profile.frameworks.contains(&"gin".to_string()));
    }

    #[test]
    fn detect_monorepo() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"name": "root", "workspaces": ["packages/*"]}"#,
        )
        .unwrap();

        let profile = detect_project_type(dir.path());
        assert!(profile.is_monorepo);
    }
}
