use super::*;
use std::fs;
use std::path::Path;

impl AgentflareMcp {
    pub(super) async fn skill_impl(&self, req: SkillRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "search" => {
                let query = req.query.unwrap_or_default();
                if query.trim().is_empty() {
                    return Err(ErrorData::invalid_params("query is required for search", None));
                }
                let mode = match req.mode.as_deref() {
                    Some("all") => skill_registry::MatchMode::All,
                    Some("any") => skill_registry::MatchMode::Any,
                    Some(other) => {
                        return Err(ErrorData::invalid_params(
                            format!("mode must be 'all' or 'any', got '{other}'"),
                            None,
                        ));
                    }
                    None => skill_registry::MatchMode::All,
                };
                let limit = req.limit.unwrap_or(5).min(50);
                let local = self.local_skills.read().await;
                let hits = if local.len() < limit {
                    let registry = tokio::task::spawn_blocking(move || {
                        skill_registry::search::search(&*self.registry.read().await, &query, limit, mode)
                    })
                    .await
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))??;
                    registry
                } else {
                    local
                        .iter()
                        .filter(|s| {
                            let q = query.to_lowercase();
                            s.name.to_lowercase().contains(&q)
                                || s.description.to_lowercase().contains(&q)
                        })
                        .take(limit)
                        .cloned()
                        .collect()
                };
                Ok(serde_json::to_string(&hits).unwrap())
            }
            "load" => {
                let name = req.name.unwrap_or_default();
                if name.trim().is_empty() {
                    return Err(ErrorData::invalid_params("name is required for load", None));
                }
                let registry = self.registry.read().await;
                let result = skill_registry::load(&registry, &name, req.original);
                match result {
                    Ok(s) => {
                        let siblings = skill_registry::load::list_siblings(&registry, &s.name, &s.source);
                        if req.activation_wrapper {
                            let json = serde_json::to_string(&s).unwrap();
                            let siblings_block = if siblings.is_empty() {
                                String::new()
                            } else {
                                format!("\n\nCompanion scripts:\n- {}", siblings.join("\n- "))
                            };
                            Ok(format!(
                                "<SKILL_ACTIVATION>\nFollow this skill definition verbatim:{json}{siblings_block}\n</SKILL_ACTIVATION>"
                            ))
                        } else {
                            Ok(serde_json::to_string(&s).unwrap())
                        }
                    }
                    Err(skill_registry::LoadError::NotFound) => {
                        let hint = skill_registry::search::search(&registry, &name, 5, skill_registry::MatchMode::Any)
                            .map(|hits| {
                                if hits.is_empty() {
                                    String::new()
                                } else {
                                    format!("\nDid you mean: {}", hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>().join(", "))
                                }
                            })
                            .unwrap_or_default();
                        Err(ErrorData::invalid_params(
                            format!("skill '{name}' not found{hint}"),
                            None,
                        ))
                    }
                    Err(e @ skill_registry::LoadError::Ambiguous(_)) => {
                        Err(ErrorData::invalid_params(e.to_string(), None))
                    }
                    Err(e) => Err(ErrorData::internal_error(e.to_string(), None)),
                }
            }
            "create" => {
                return Self::skill_create(req).await;
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }

    async fn skill_create(req: SkillRequest) -> Result<String, ErrorData> {
        let name = req.name.ok_or_else(|| ErrorData::invalid_params("name is required for create", None))?;
        let template = req.template.unwrap_or_else(|| "base".to_string());
        let description = req.description.unwrap_or_else(|| format!("Custom skill: {name}"));
        let tags = req.tags.unwrap_or_default();

        if name.trim().is_empty() {
            return Err(ErrorData::invalid_params("name cannot be empty", None));
        }

        let slug = Self::slugify(&name);
        let skills_dir = dirs::home_dir()
            .ok_or_else(|| ErrorData::internal_error("Could not determine home directory", None))?
            .join(".agentflare")
            .join("skills");

        fs::create_dir_all(&skills_dir)
            .map_err(|e| ErrorData::internal_error(format!("Failed to create skills directory: {e}"), None))?;

        let skill_dir = skills_dir.join(&slug);
        if skill_dir.exists() {
            return Err(ErrorData::invalid_params(
                format!("Skill '{name}' already exists at {}", skill_dir.display()),
                None,
            ));
        }

        fs::create_dir_all(&skill_dir)
            .map_err(|e| ErrorData::internal_error(format!("Failed to create skill directory: {e}"), None))?;

        let skill_content = Self::render_template(&template, &name, &description, &tags);
        let skill_path = skill_dir.join("SKILL.md");
        fs::write(&skill_path, skill_content)
            .map_err(|e| ErrorData::internal_error(format!("Failed to write skill file: {e}"), None))?;

        Ok(format!("Created skill '{}' at {}", name, skill_path.display()))
    }

    fn render_template(template: &str, name: &str, description: &str, tags: &[String]) -> String {
        let tags_str = if tags.is_empty() {
            String::new()
        } else {
            format!("\ntags: {}", tags.join(", "))
        };

        match template {
            "web-development" => Self::template_web_development(name, description, &tags_str),
            "api-development" => Self::template_api_development(name, description, &tags_str),
            "testing" => Self::template_testing(name, description, &tags_str),
            _ => Self::template_base(name, description, &tags_str),
        }
    }

    fn template_base(name: &str, description: &str, tags: &str) -> String {
        format!(r#"---\nname: {name}\ndescription: {description}{tags}\n---\n\n# {name}\n\n{description}\n\n## When to use\n\nUse this skill when...\n\n## Instructions\n\n1. First, understand the task\n2. Then, implement the solution\n3. Finally, verify the result\n"#)
    }

    fn template_web_development(name: &str, description: &str, tags: &str) -> String {
        format!(r#"---\nname: {name}\ndescription: {description}{tags}\n---\n\n# {name}\n\n{description}\n\n## When to use\n\nUse for web development tasks: frontend, backend, full-stack.\n\n## Instructions\n\n1. Analyze the requirements\n2. Choose appropriate framework/tools\n3. Implement with best practices\n4. Test across browsers/devices\n5. Optimize for performance\n\n## References\n\n- MDN Web Docs\n- Framework-specific documentation\n"#)
    }

    fn template_api_development(name: &str, description: &str, tags: &str) -> String {
        format!(r#"---\nname: {name}\ndescription: {description}{tags}\n---\n\n# {name}\n\n{description}\n\n## When to use\n\nUse for API development: REST, GraphQL, gRPC.\n\n## Instructions\n\n1. Define the API contract\n2. Implement endpoints\n3. Add validation and error handling\n4. Write tests\n5. Document the API\n\n## References\n\n- OpenAPI/Swagger spec\n- API design best practices\n"#)
    }

    fn template_testing(name: &str, description: &str, tags: &str) -> String {
        format!(r#"---\nname: {name}\ndescription: {description}{tags}\n---\n\n# {name}\n\n{description}\n\n## When to use\n\nUse for testing tasks: unit, integration, e2e testing.\n\n## Instructions\n\n1. Identify what needs testing\n2. Choose appropriate test framework\n3. Write test cases\n4. Run tests and verify coverage\n5. Maintain test suite\n\n## References\n\n- Testing pyramid\n- Framework-specific testing guides\n"#)
    }

    fn slugify(name: &str) -> String {
        name.to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }
}