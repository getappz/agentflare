//! `skill` MCP tool handler body — split out of mcp_server.rs, mirroring
//! item.rs/claim.rs/comment.rs's one-file-per-tool convention.

use super::*;

impl AgentflareMcp {
    pub(super) async fn skill_impl(&self, req: SkillRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "search" => {
                let query = req
                    .query
                    .ok_or_else(|| ErrorData::invalid_params("query is required", None))?;
                if query.trim().is_empty() {
                    return Err(ErrorData::invalid_params("query is required", None));
                }
                let mode = match req.mode.as_deref() {
                    None | Some("all") => skill_registry::MatchMode::All,
                    Some("any") => skill_registry::MatchMode::Any,
                    Some(other) => {
                        return Err(ErrorData::invalid_params(
                            format!("mode must be 'all' or 'any', got '{other}'"),
                            None,
                        ));
                    }
                };
                let limit = req.limit.unwrap_or(5);
                // Lazily backfill a small batch of missing embeddings before
                // searching, so the semantic index gradually catches up on
                // first use rather than needing a separate background job.
                // A no-op (and free) when the `semantic` feature is off.
                let _ = self.with_fresh_registry(|reg| {
                    reg.backfill_embeddings(
                        crate::memory::engine::embed_doc,
                        crate::memory::engine::model_name().as_deref().unwrap_or(""),
                        25,
                    )
                });
                // No explicit mode: all-words, then any-word if that finds
                // nothing. An explicit "all" stays strict.
                let relaxed = req.mode.is_none();
                let local = self
                    .with_fresh_registry(|reg| {
                        if relaxed {
                            reg.search_semantic_relaxed(
                                &query,
                                limit,
                                crate::memory::engine::embed_query,
                            )
                        } else {
                            reg.search_semantic(
                                &query,
                                limit,
                                mode,
                                crate::memory::engine::embed_query,
                            )
                        }
                    })?
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                // "No good local match": weak hits are dropped so the external
                // fallbacks below get the chance to fill the open slots.
                let mut local: Vec<_> = local.into_iter().filter(|h| h.is_confident()).collect();
                // Configured local registries take at most half of the open
                // slots: their word match carries no confidence score, so they
                // must not starve the skills.sh fallback. Remote registries
                // would need a git clone per search and are skipped here.
                let room = limit.saturating_sub(local.len()).div_ceil(2);
                if room > 0 {
                    let registry =
                        crate::cli::skill::catalog_hits(&query, limit, false, false).await;
                    let mut added = 0;
                    for hit in registry {
                        if added == room {
                            break;
                        }
                        if !local.iter().any(|h| h.name == hit.name) {
                            local.push(hit);
                            added += 1;
                        }
                    }
                }
                let hits = if skill_registry::skills_sh::needs_external(&local, limit) {
                    let remaining = limit.saturating_sub(local.len());
                    let (q_registry, q_skills_sh) = (query.clone(), query.clone());
                    let (registry, skills_sh) = tokio::join!(
                        tokio::task::spawn_blocking(move || {
                            gateway_registry::registry_search::search_registry(
                                &q_registry,
                                remaining.min(skill_registry::skills_sh::MAX_REMOTE_LIMIT),
                            )
                        }),
                        tokio::task::spawn_blocking(move || {
                            skill_registry::skills_sh::search_skills_sh(&q_skills_sh, remaining)
                        }),
                    );
                    let registry = registry.unwrap_or_default();
                    let skills_sh = skills_sh.unwrap_or_default();
                    // Both sources are fallbacks: split the open slots so
                    // neither can starve the other. The user-configured
                    // registry takes the first slot when the count is odd;
                    // skills.sh fills the rest and any surplus it leaves.
                    let registry_reserved = registry.len().min(remaining.div_ceil(2));
                    let skills_sh = skills_sh
                        .into_iter()
                        .take(remaining - registry_reserved)
                        .collect();
                    let hits =
                        skill_registry::skills_sh::merge_external_hits(local, limit, skills_sh);
                    dedupe_hits(skill_registry::merge_registry_hits(hits, limit, registry))
                } else {
                    local
                };
                let json = serde_json::to_string_pretty(&hits).unwrap_or_default();
                // Don't silently degrade to keyword-only ranking.
                Ok(with_semantic_warning(
                    json,
                    crate::memory::engine::unavailable_reason(),
                ))
            }
            "load" => {
                let name = req
                    .name
                    .ok_or_else(|| ErrorData::invalid_params("name is required", None))?;
                if name.trim().is_empty() {
                    return Err(ErrorData::invalid_params("name is required", None));
                }
                let mut installed_from = None;
                let mut github_install = false;
                let name = if crate::cli::skill::github_source(&name).is_some() {
                    let (resolved, source) = crate::cli::skill::ensure_installed(&name, false)
                        .await
                        .map_err(|e| ErrorData::invalid_params(e.to_string(), None))?;
                    installed_from = source;
                    github_install = true;
                    format!("agentflare-user:{resolved}")
                } else {
                    name
                };
                let mut result = self.with_fresh_registry(|reg| reg.load(&name, req.original))?;
                // Not indexed: force-rescan once (a CLI install may have landed
                // inside the debounce window), then try the configured
                // registries, staging into ~/.agentflare/skills (never an
                // agent dir), and force-rescan again so the new files show up.
                if matches!(result, Err(skill_registry::LoadError::NotFound(_))) {
                    result =
                        self.with_forced_fresh_registry(|reg| reg.load(&name, req.original))?;
                }
                if matches!(result, Err(skill_registry::LoadError::NotFound(_)))
                    && crate::cli::skill::is_plain_skill_name(&name)
                {
                    match crate::cli::skill::ensure_installed(&name, false).await {
                        Ok((resolved, source)) => {
                            installed_from = source;
                            result = self.with_forced_fresh_registry(|reg| {
                                reg.load(&format!("agentflare-user:{resolved}"), req.original)
                            })?;
                        }
                        Err(::skill::error::SkillError::SkillNotFound(_)) => {}
                        Err(e) => {
                            return Err(ErrorData::internal_error(
                                format!(
                                    "skill '{name}' is not indexed and auto-install from the configured registries failed: {e}"
                                ),
                                None,
                            ));
                        }
                    }
                }
                match result {
                    Ok(s) => {
                        // Tell the caller when this call fetched the skill, and from where.
                        let mut value = serde_json::to_value(&s).unwrap_or_default();
                        if let (Some(registry), Some(obj)) =
                            (&installed_from, value.as_object_mut())
                        {
                            let field = if github_install {
                                "installed_from"
                            } else {
                                "installed_from_registry"
                            };
                            obj.insert(field.into(), registry.clone().into());
                        }
                        let json = serde_json::to_string_pretty(&value).unwrap_or_default();
                        if req.activation_wrapper {
                            let siblings: Vec<String> = s
                                .siblings
                                .iter()
                                .map(|p| p.to_string_lossy().to_string())
                                .collect();
                            let siblings_block = if siblings.is_empty() {
                                String::new()
                            } else {
                                format!("\n\nCompanion scripts:\n- {}", siblings.join("\n- "))
                            };
                            Ok(format!(
                                "<SKILL_ACTIVATION>\nFollow this skill definition verbatim:{json}{siblings_block}\n</SKILL_ACTIVATION>"
                            ))
                        } else {
                            Ok(json)
                        }
                    }
                    Err(e @ skill_registry::LoadError::NotFound(_))
                    | Err(e @ skill_registry::LoadError::Ambiguous(_)) => {
                        Err(ErrorData::invalid_params(e.to_string(), None))
                    }
                    Err(e) => Err(ErrorData::internal_error(e.to_string(), None)),
                }
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }
}

impl AgentflareMcp {
    pub(super) async fn skill_create_impl(
        &self,
        req: SkillCreateRequest,
    ) -> Result<String, ErrorData> {
        use std::fs;
        use std::path::PathBuf;

        // Validate name
        if !crate::cli::skill::is_plain_skill_name(&req.name) {
            return Err(ErrorData::invalid_params(
                "expected a plain skill name",
                None,
            ));
        }

        // Determine template
        let template = req.template.unwrap_or_else(|| "base".to_string());

        // Get template content
        let (_frontmatter, body) = match Self::get_template(&template) {
            Some((fm, b)) => (fm, b),
            None => {
                return Err(ErrorData::invalid_params(
                    format!(
                        "unknown template: {}. Available: web-development, api-development, testing, base",
                        template
                    ),
                    None,
                ));
            }
        };

        // Determine target directory
        let target_dir = req.target_dir.unwrap_or_else(|| {
            crate::cli::skill::home_skills()
                .to_string_lossy()
                .to_string()
        });
        let target_path = PathBuf::from(&target_dir).join(&req.name);

        // Check if already exists
        if target_path.exists() {
            return Err(ErrorData::invalid_params(
                format!("skill directory already exists: {}", target_path.display()),
                None,
            ));
        }

        // Create directory
        fs::create_dir_all(&target_path).map_err(|e| {
            ErrorData::internal_error(format!("failed to create skill directory: {e}"), None)
        })?;

        // Build frontmatter
        let description = req
            .description
            .unwrap_or_else(|| format!("{} skill", req.name));
        let tags = if req.tags.is_empty() {
            String::new()
        } else {
            format!(
                "\ntags: [{}]",
                req.tags
                    .iter()
                    .map(|t| format!("\"{}\"", t))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let fm_content = format!(
            "---\nname: {}\ndescription: {}{}\n---\n",
            req.name, description, tags
        );

        // Write SKILL.md
        let skill_file = target_path.join("SKILL.md");
        let full_content = format!("{}{}", fm_content, body);
        fs::write(&skill_file, full_content).map_err(|e| {
            ErrorData::internal_error(format!("failed to write SKILL.md: {e}"), None)
        })?;

        Ok(serde_json::json!({
            "created": true,
            "path": skill_file.to_string_lossy(),
            "name": req.name,
            "template": template
        })
        .to_string())
    }

    fn get_template(name: &str) -> Option<(String, String)> {
        match name {
            "web-development" => Some((
                String::new(),
                r#"## When to use
Use for web development tasks including frontend, backend, and full-stack work.

## Frontend
- React, Vue, Svelte component patterns
- State management (Redux, Zustand, Pinia, etc.)
- CSS/SCSS/Tailwind styling approaches
- TypeScript type safety

## Backend
- API design (REST, GraphQL, tRPC)
- Database schema and migrations
- Authentication and authorization
- Caching strategies

## Testing
- Unit, integration, E2E testing patterns
- Test-driven development workflows
"#
                .to_string(),
            )),
            "api-development" => Some((
                String::new(),
                r#"## When to use
Use for API development tasks including design, implementation, and documentation.

## Design
- RESTful resource modeling
- GraphQL schema design
- tRPC/grpc contract-first development
- Versioning strategies

## Implementation
- Request/response validation
- Error handling patterns
- Rate limiting and throttling
- Observability (logging, metrics, tracing)

## Documentation
- OpenAPI/Swagger generation
- API versioning and deprecation
- Client SDK considerations
"#
                .to_string(),
            )),
            "testing" => Some((
                String::new(),
                r#"## When to use
Use for testing strategies, patterns, and best practices.

## Unit Testing
- Test organization and naming
- Mocking and stubbing strategies
- Property-based testing
- Snapshot testing

## Integration Testing
- Database integration tests
- API contract testing
- Service-to-service testing
- Testcontainers patterns

## E2E Testing
- Browser automation (Playwright, Cypress)
- User journey testing
- Visual regression testing
- CI/CD integration
"#
                .to_string(),
            )),
            "base" => Some((
                String::new(),
                r#"## When to use
Use for general-purpose skill scaffolding. Replace this content with your skill-specific guidance.

## Structure
- When to use this skill
- Key concepts and patterns
- Best practices
- Common pitfalls
- Handoff instructions
"#
                .to_string(),
            )),
            _ => None,
        }
    }
}

/// Drop later hits that repeat a name, keeping the first (local, then registry,
/// then skills.sh) so a skill never appears twice with conflicting hints.
fn dedupe_hits(hits: Vec<skill_registry::SkillHit>) -> Vec<skill_registry::SkillHit> {
    let mut seen = std::collections::HashSet::new();
    hits.into_iter()
        .filter(|h| seen.insert(h.name.clone()))
        .collect()
}

/// Prefix `json` with a one-line warning when semantic search is unavailable,
/// so keyword-only ranking is never silent.
fn with_semantic_warning(json: String, unavailable: Option<String>) -> String {
    match unavailable {
        Some(why) => format!(
            "warning: semantic skill search unavailable ({why}); results are keyword-only
{json}"
        ),
        None => json,
    }
}

#[cfg(test)]
mod semantic_warning_tests {
    use super::with_semantic_warning;

    #[test]
    fn warns_when_semantic_unavailable_and_passes_json_through_otherwise() {
        let out = with_semantic_warning("[]".into(), Some("no model".into()));
        assert!(out.starts_with("warning: semantic skill search unavailable (no model)"));
        assert!(out.ends_with(
            "
[]"
        ));
        assert_eq!(with_semantic_warning("[]".into(), None), "[]");
    }
}

#[cfg(test)]
mod dedupe_tests {
    use super::dedupe_hits;

    fn hit(name: &str, source: &str) -> skill_registry::SkillHit {
        skill_registry::SkillHit::external(name, source, "", None, None)
    }

    #[test]
    fn keeps_first_hit_for_each_name() {
        let hits = dedupe_hits(vec![
            hit("pm", "local"),
            hit("pm", "mcp_registry"),
            hit("other", "skills.sh"),
        ]);
        let seen: Vec<_> = hits
            .iter()
            .map(|h| (h.name.as_str(), h.source.as_str()))
            .collect();
        assert_eq!(seen, [("pm", "local"), ("other", "skills.sh")]);
    }
}
