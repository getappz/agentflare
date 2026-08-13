use super::*;

impl AgentflareMcp {
    pub(crate) fn project_link_path(&self) -> std::path::PathBuf {
        self.backend_project_link_override
            .clone()
            .unwrap_or_else(|| {
                Self::repo_root()
                    .join(Self::LINK_MARKER)
                    .join("project.json")
            })
    }

    /// Derives a project name from the git remote (`getappz/agentflare` →
    /// `agentflare`) or, outside a repo, the directory basename.
    pub(crate) fn resolve_project_name() -> String {
        if let Some(repo) = Self::run_git(&["remote", "get-url", "origin"]) {
            let normalized = crate::claims::normalize_repo(&repo);
            if let Some(name) = normalized.rsplit('/').next().filter(|s| !s.is_empty()) {
                return name.to_string();
            }
        }
        std::env::current_dir()
            .ok()
            .and_then(|d| d.file_name().map(|n| n.to_string_lossy().to_string()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".to_string())
    }

    /// Short uppercase alnum identifier for a project (used for issue-key
    /// prefixes like `AGENTFLARE-42`).
    fn derive_project_identifier(name: &str) -> String {
        let ident: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_uppercase();
        if ident.is_empty() {
            "PROJ".to_string()
        } else {
            ident.chars().take(10).collect()
        }
    }

    /// The one and only workspace on this system: reused if it already
    /// exists, auto-created (named "default") on first use. Never exposed
    /// as an MCP parameter.
    pub(crate) fn resolve_workspace_id(
        conn: &rusqlite::Connection,
    ) -> crate::errors::Result<String> {
        let existing: Option<String> = conn
            .query_row(
                "SELECT id FROM workspaces WHERE deleted_at IS NULL ORDER BY created_at LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        let ws = agentflare_backend::workspace::create(
            conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "default".to_string(),
                slug: "default".to_string(),
                owner_agent: None,
                item_label: None,
            },
        )?;
        Ok(ws.id)
    }

    /// Marks a project as auto-provisioned by this resolver, in `external_source`.
    const REPO_EXTERNAL_SOURCE: &'static str = "agentflare-repo";

    /// Stable identity key for "this repo" — normalized git remote when
    /// available (so multiple clones/worktrees of the same remote share one
    /// project, matching `claims.rs`'s own repo-key model), else the
    /// canonicalized repo root path. Deliberately NOT the derived display
    /// name/identifier: two unrelated directories can easily share a
    /// basename (`~/work/foo` and `~/scratch/foo`), and conflating them
    /// would silently merge one project's items into the other's.
    fn resolve_repo_key(&self) -> String {
        if let Some(key) = self.backend_repo_key_override.clone() {
            return key;
        }
        if let Some(remote) = Self::run_git(&["remote", "get-url", "origin"]) {
            return format!("git:{}", crate::claims::normalize_repo(&remote));
        }
        let root = Self::repo_root();
        // `dunce`, not `std::fs::canonicalize` directly: on Windows std adds
        // a `\\?\` UNC prefix that Git for Windows' MSYS layer can't handle
        // when this path is later fed to `git worktree add` (folder_path
        // flows into dispatched jobs via `register_project_dir` below) —
        // same rationale as `code::impact_for_path`'s use of `dunce`.
        let canonical = dunce::canonicalize(&root).unwrap_or(root);
        format!("path:{}", canonical.to_string_lossy())
    }

    /// The `path:`-style key `resolve_repo_key` would have returned before
    /// it switched from `std::fs::canonicalize` to `dunce::canonicalize`.
    /// `resolve_project` also matches against this so a pre-existing
    /// local-only (no git remote) project registered under the old
    /// `\\?\`-prefixed key on Windows is reconnected instead of duplicated.
    /// `None` whenever `resolve_repo_key` wouldn't have taken the `path:`
    /// branch anyway (override set, or a git remote exists).
    fn legacy_repo_key(&self) -> Option<String> {
        if self.backend_repo_key_override.is_some() {
            return None;
        }
        if Self::run_git(&["remote", "get-url", "origin"]).is_some() {
            return None;
        }
        let root = Self::repo_root();
        let canonical = std::fs::canonicalize(&root).unwrap_or(root);
        Some(format!("path:{}", canonical.to_string_lossy()))
    }

    /// Vercel-style auto-link: reads `.agentflare/project.json` at
    /// the repo root if present; otherwise derives a project from git/cwd
    /// context and creates or reconnects to it. Reconnects rather than
    /// duplicates when
    /// the link file is missing but this repo's project already exists
    /// (deleted link file, wiped worktree, etc.) — matched by
    /// `resolve_repo_key()`, not by the derived display identifier, so two
    /// differently-located repos that happen to share a name are never
    /// conflated; the identifier only gets a disambiguating suffix.
    pub(crate) fn resolve_project(
        &self,
        conn: &rusqlite::Connection,
    ) -> Result<agentflare_backend::project::Project, ErrorData> {
        let link_path = self.project_link_path();
        if let Ok(bytes) = std::fs::read(&link_path)
            && let Ok(link) = serde_json::from_slice::<ProjectLink>(&bytes)
        {
            match agentflare_backend::project::get(conn, &link.project_id) {
                Ok(project) => {
                    self.register_bridge_repo(conn, &project.id);
                    self.register_project_dir(conn, &project.id);
                    return Ok(project);
                }
                Err(agentflare_backend::Error::NotFound(_)) => {} // stale link — re-resolve below
                Err(e) => return Err(map_backend_err(e)),
            }
        }

        let workspace_id = Self::resolve_workspace_id(conn)?;
        let name = Self::resolve_project_name();
        let identifier = Self::derive_project_identifier(&name);
        let repo_key = self.resolve_repo_key();
        let legacy_repo_key = self.legacy_repo_key();

        let existing = agentflare_backend::project::list_by_workspace(conn, &workspace_id)
            .map_err(map_backend_err)?
            .into_iter()
            .find(|p| {
                p.external_source.as_deref() == Some(Self::REPO_EXTERNAL_SOURCE)
                    && (p.external_id.as_deref() == Some(repo_key.as_str())
                        || (legacy_repo_key.is_some()
                            && p.external_id.as_deref() == legacy_repo_key.as_deref()))
            });
        let project = if let Some(project) = existing {
            project
        } else {
            let mut attempt = 0u32;
            loop {
                let suffix = if attempt == 0 {
                    String::new()
                } else {
                    format!("-{}", attempt + 1)
                };
                match agentflare_backend::project::create(
                    conn,
                    agentflare_backend::project::CreateProject {
                        workspace_id: workspace_id.clone(),
                        name: format!("{name}{suffix}"),
                        identifier: format!("{identifier}{suffix}"),
                        external_source: Some(Self::REPO_EXTERNAL_SOURCE.to_string()),
                        external_id: Some(repo_key.clone()),
                    },
                ) {
                    Ok(p) => break p,
                    Err(agentflare_backend::Error::Duplicate(_)) if attempt < 20 => {
                        attempt += 1;
                    }
                    Err(e) => return Err(map_backend_err(e)),
                }
            }
        };

        let link = ProjectLink {
            workspace_id,
            project_id: project.id.clone(),
            identifier: project.identifier.clone(),
        };
        if let Some(parent) = link_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &link_path,
            serde_json::to_vec_pretty(&link).unwrap_or_default(),
        );
        self.register_bridge_repo(conn, &project.id);
        self.register_project_dir(conn, &project.id);
        Ok(project)
    }

    /// Resolves the project a read-only reporting action should run against:
    /// the `project` override (name, case-insensitive, or UUID — looked up in
    /// the linked workspace) when given, else the repo's linked project.
    /// Lookup-only for overrides: none of `resolve_project`'s link-file /
    /// bridge-registration side effects apply to a project this repo merely
    /// reports on.
    pub(crate) fn resolve_project_for_read(
        &self,
        conn: &rusqlite::Connection,
        project_override: Option<&str>,
    ) -> Result<agentflare_backend::project::Project, ErrorData> {
        let Some(wanted) = project_override.map(str::trim).filter(|s| !s.is_empty()) else {
            return self.resolve_project(conn);
        };
        let workspace_id = Self::resolve_workspace_id(conn)?;
        let projects = agentflare_backend::project::list_by_workspace(conn, &workspace_id)
            .map_err(map_backend_err)?;
        projects
            .into_iter()
            .find(|p| p.id == wanted || p.name.eq_ignore_ascii_case(wanted))
            .ok_or_else(|| {
                ErrorData::invalid_params(
                    format!(
                        "project '{wanted}' not found in the linked workspace — \
                         use `project action=list` to see valid names/ids"
                    ),
                    None,
                )
            })
    }

    /// Refreshes this repo's row in the local GitHub bridge's repo registry
    /// (`bridge_repos`) — the reverse of `project.json`'s folder→project
    /// link, indexed by repo instead so the daemon (no reliable cwd, so it
    /// cannot read `project.json` itself) can enumerate every repo it should
    /// poll. A no-op when `origin` isn't a GitHub remote (e.g. a local-only
    /// or non-GitHub-hosted repo): nothing to bridge, so nothing to
    /// register. Best-effort — a registry write failure must not break
    /// project resolution, which every MCP/CLI call in this repo depends on.
    fn register_bridge_repo(&self, conn: &rusqlite::Connection, project_id: &str) {
        let repo_root = self.worktree_repo_root();
        let Some(repo_id) = crate::github::RepoId::resolve_from_remote(&repo_root) else {
            return;
        };
        // `dunce`, not `std::fs::canonicalize` directly: on Windows std adds
        // a `\\?\` UNC prefix that Git for Windows' MSYS layer can't handle
        // once this path reaches `git worktree add` for a dispatched job.
        let folder_path = dunce::canonicalize(&repo_root).unwrap_or(repo_root);
        let queue_label = crate::github::bridge::config::resolve_project_queue_label(&folder_path);
        let _ = agentflare_backend::bridge_repo::upsert(
            conn,
            &repo_id.to_string(),
            project_id,
            &folder_path.to_string_lossy(),
            &queue_label,
            None,
            crate::claims::now(),
        );
    }

    /// Refreshes this repo's row in the general project-directory registry
    /// (`project_dirs`) — the reverse of `project.json`'s folder→project
    /// link, indexed by project instead so a process with no reliable cwd of
    /// its own (the daemon's supervisor discovery loop) can enumerate every
    /// project's folder it should operate against. Unlike
    /// `register_bridge_repo`, not gated on a GitHub remote: every linked
    /// project gets a row here. Best-effort — a registry write failure must
    /// not break project resolution, which every MCP/CLI call depends on.
    fn register_project_dir(&self, conn: &rusqlite::Connection, project_id: &str) {
        let repo_root = self.worktree_repo_root();
        // `dunce`, not `std::fs::canonicalize` directly: on Windows std adds
        // a `\\?\` UNC prefix that breaks `git worktree add` once this path
        // is threaded through to a dispatched job as its `folder_path` (see
        // `worktree::create_worktree`, which is what actually spawns that
        // command).
        let folder_path = dunce::canonicalize(&repo_root).unwrap_or(repo_root);
        let _ = agentflare_backend::project_dir::upsert(
            conn,
            project_id,
            &folder_path.to_string_lossy(),
            crate::claims::now(),
        );
    }
}
