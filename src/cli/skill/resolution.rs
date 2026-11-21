use super::*;
use skill::types::{ParsedSource, Skill, SourceType};

pub(crate) fn github_source(input: &str) -> Option<ParsedSource> {
    let input = input.trim_end_matches('/');
    if !(input.starts_with("https://github.com/")
        || input.starts_with("gh:")
        || input.starts_with("github:"))
    {
        return None;
    }
    let input = input
        .strip_suffix("/SKILL.md")
        .unwrap_or(input)
        .replacen("/blob/", "/tree/", 1);
    let parsed = parse_source(&normalize_source_shorthand(&input));
    (parsed.source_type == SourceType::Github).then_some(parsed)
}

pub(super) async fn entries(
    source: &ParsedSource,
) -> Result<(Option<tempfile::TempDir>, Vec<Skill>), SkillError> {
    let checkout = if source.source_type == SourceType::Local {
        None
    } else {
        Some(
            clone_repo(&source.url, source.git_ref.as_deref())
                .await
                .map_err(redact_skill_error)?,
        )
    };
    let root = checkout
        .as_ref()
        .map(|dir| dir.path())
        .or(source.local_path.as_deref())
        .ok_or_else(|| SkillError::InvalidSource("missing skill source directory".into()))?;
    let options = DiscoverOptions {
        full_depth: source.subpath.is_none(),
        ..Default::default()
    };
    let skills = discover_skills(root, source.subpath.as_deref(), &options).await?;
    let root = root.canonicalize().map_err(|e| SkillError::io(root, e))?;
    for skill in &skills {
        let file = skill.path.join("SKILL.md");
        if !file
            .canonicalize()
            .map_err(|e| SkillError::io(&file, e))?
            .starts_with(&root)
        {
            return Err(SkillError::PathTraversal {
                context: "skill source",
                path: file.display().to_string(),
            });
        }
    }
    Ok((checkout, skills))
}

// Publish a complete directory on the same filesystem, including companion files.
pub(super) fn stage(skill: &Skill) -> Result<(), SkillError> {
    if !is_plain_skill_name(&skill.name) {
        return Err(SkillError::InvalidSource(
            "invalid skill name in frontmatter".into(),
        ));
    }
    let root = home_skills();
    std::fs::create_dir_all(&root).map_err(|e| SkillError::io(&root, e))?;
    let tmp = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(&root)
        .map_err(|e| SkillError::io(&root, e))?;
    copy_contents(&skill.path, tmp.path()).map_err(|e| SkillError::io(&skill.path, e))?;
    let dest = root.join(&skill.name);
    match std::fs::rename(tmp.path(), &dest) {
        Ok(()) => Ok(()),
        Err(_) if dest.join("SKILL.md").is_file() => Ok(()),
        Err(e) => Err(SkillError::io(dest, e)),
    }
}

fn copy_contents(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.')
            || matches!(
                name.as_ref(),
                "__pycache__" | "__pypackages__" | "metadata.json"
            )
        {
            continue;
        }
        let dest = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            std::fs::create_dir(&dest)?;
            copy_contents(&entry.path(), &dest)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), dest)?;
        } else {
            return Err(std::io::Error::other(format!(
                "unsupported skill entry: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

async fn by_name(name: &str) -> Result<Option<(String, Option<String>)>, SkillError> {
    if !is_plain_skill_name(name) {
        return Err(SkillError::InvalidSource(
            "expected a plain skill name or a GitHub skill URL".into(),
        ));
    }
    if home_skills().join(name).join("SKILL.md").is_file() {
        return Ok(Some((name.into(), None)));
    }
    Ok(install_from_registries(name)
        .await?
        .map(|source| (name.into(), Some(source))))
}

/// Return the installed name and, only on a new installation, its source.
pub(crate) async fn ensure_installed(input: &str) -> Result<(String, Option<String>), SkillError> {
    let Some(source) = github_source(input) else {
        let miss = match by_name(input).await {
            Ok(Some(hit)) => return Ok(hit),
            Ok(None) => SkillError::SkillNotFound(input.into()),
            Err(e) if !is_plain_skill_name(input) => return Err(e),
            Err(e) => e,
        };
        let name = input.to_owned();
        let remote = tokio::task::spawn_blocking(move || {
            skill_registry::skills_sh::exact_github_source(&name)
        })
        .await
        .map_err(|e| SkillError::InvalidSource(e.to_string()))?;
        if let Some(repo) = remote
            && try_install_from_source(&repo, input).await?
        {
            return Ok((input.into(), Some(repo)));
        }
        return Err(miss);
    };
    let hint = source
        .skill_filter
        .as_deref()
        .or_else(|| source.subpath.as_deref()?.rsplit('/').next())
        .or_else(|| source.url.trim_end_matches(".git").rsplit('/').next())
        .unwrap_or("");
    if !is_plain_skill_name(hint) {
        return Err(SkillError::InvalidSource(
            "invalid GitHub skill name".into(),
        ));
    }
    if let Some(hit) = registry_or_fallback(hint).await {
        return Ok(hit);
    }

    let (_checkout, skills) = entries(&source).await?;
    let selected = match skills.iter().find(|s| s.name == hint) {
        Some(skill) => skill,
        None if source.skill_filter.is_none() && skills.len() == 1 => &skills[0],
        None if skills.len() > 1 => {
            let names = skills
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(SkillError::InvalidSource(format!(
                "select a skill with github:OWNER/REPO@NAME; available: {names}"
            )));
        }
        None => return Err(SkillError::SkillNotFound(hint.into())),
    };
    if selected.name != hint
        && let Some(hit) = registry_or_fallback(&selected.name).await
    {
        return Ok(hit);
    }
    stage(selected)?;
    Ok((selected.name.clone(), Some(source.url)))
}

async fn registry_or_fallback(name: &str) -> Option<(String, Option<String>)> {
    match by_name(name).await {
        Ok(hit) => hit,
        Err(e) => {
            eprintln!("registry lookup failed; trying the supplied GitHub source: {e}");
            None
        }
    }
}

pub(crate) async fn catalog_hits(
    query: &str,
    limit: usize,
    include_installed: bool,
) -> Vec<skill_registry::SkillHit> {
    let words: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
    let mut hits = Vec::new();
    if words.is_empty() || limit == 0 {
        return hits;
    }
    let mut seen = HashSet::new();
    let mut sources = load_registries();
    if include_installed && home_skills().is_dir() {
        sources.insert(0, home_skills().to_string_lossy().into_owned());
    }
    for source in sources {
        match entries(&parse_source(&normalize_source_shorthand(&source))).await {
            Ok((_checkout, skills)) => {
                for skill in skills {
                    let text = format!("{} {}", skill.name, skill.description).to_lowercase();
                    if is_plain_skill_name(&skill.name)
                        && words.iter().all(|word| text.contains(word))
                        && seen.insert(skill.name.clone())
                    {
                        let hint = format!("agentflare skills get {}", skill.name);
                        hits.push(skill_registry::SkillHit::external(
                            skill.name,
                            &redact_url_credentials(&source),
                            skill.description,
                            Some(hint),
                            None,
                        ));
                        if hits.len() == limit {
                            return hits;
                        }
                    }
                }
            }
            Err(error) => eprintln!("{}: {error}", redact_url_credentials(&source)),
        }
    }
    hits
}

pub(super) async fn search(query: &str) {
    let mut hits = catalog_hits(query, usize::MAX, true).await;
    if hits.is_empty() {
        hits = skill_registry::skills_sh::search_skills_sh(query, 10);
    }
    if hits.is_empty() {
        crate::ui::info("no matching skills found");
    }
    for hit in hits {
        println!(
            "{} — {}\n  {}",
            hit.name,
            hit.description,
            hit.install_hint.unwrap_or_default()
        );
    }
}
