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
                let local = self
                    .with_fresh_registry(|reg| {
                        reg.search_semantic(&query, limit, mode, crate::memory::engine::embed_query)
                    })?
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let hits = if local.len() < limit {
                    let remaining = limit - local.len();
                    let query_owned = query.clone();
                    let registry = tokio::task::spawn_blocking(move || {
                        gateway_registry::registry_search::search_registry(&query_owned, remaining)
                    })
                    .await
                    .unwrap_or_default();
                    skill_registry::merge_registry_hits(local, limit, registry)
                } else {
                    local
                };
                Ok(serde_json::to_string_pretty(&hits).unwrap_or_default())
            }
            "load" => {
                let name = req
                    .name
                    .ok_or_else(|| ErrorData::invalid_params("name is required", None))?;
                if name.trim().is_empty() {
                    return Err(ErrorData::invalid_params("name is required", None));
                }
                let result = self.with_fresh_registry(|reg| reg.load(&name, req.original))?;
                match result {
                    Ok(s) => {
                        let json = serde_json::to_string_pretty(&s).unwrap_or_default();
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
        if req.name.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "skill name cannot be empty",
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
            Self::repo_root()
                .join(".claude/skills")
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
