//! MCP (Model Context Protocol) server over stdio, built on the `rmcp` crate
//! (`modelcontextprotocol/rust-sdk`, published to crates.io — a normal
//! dependency, not ported code; no /NOTICE entry needed).

mod artifact;
mod asset;
mod claim;
mod comment;
mod flare_git;
mod handoff;
pub(crate) mod item;
mod memory_tool;
mod review;
pub(crate) mod types;

use crate::optimize;
use crate::progress::{PROGRESS_SENDER, ProgressSender};
use base64::Engine as _;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{tool::ToolCallContext, wrapper::Parameters},
    model::{
        AnnotateAble, CallToolRequestParams, CallToolResult, ErrorData, GetPromptRequestParams,
        GetPromptResult, Implementation, ListPromptsResult, ListResourcesResult, Meta,
        PaginatedRequestParams, RawResource, ReadResourceRequestParams, ReadResourceResult,
        ResourceContents, ServerCapabilities, ServerInfo,
    },
    schemars,
    service::{RequestContext, RoleServer},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use rusqlite::OptionalExtension;
use serde::Deserialize;

use types::*;

#[derive(Default)]
pub struct AgentflareMcp {
    /// Persisted across calls so `Registry::ensure_fresh`'s 60s debounce is
    /// real: a fresh `Registry` per call would rescan + rebuild every time.
    /// `Registry` owns a `rusqlite::Connection` (Send, not Sync); the mutex
    /// makes the server type `Sync` without requiring `Registry` to be.
    skills_registry: std::sync::Mutex<Option<skill_registry::Registry>>,
    /// Tests inject a temp path here so they never touch the shared skills.db.
    skills_db_override: Option<std::path::PathBuf>,
    /// `tokio::sync::Mutex`, not `std::sync::Mutex` like `skills_registry` —
    /// `gateway_registry::Registry`'s methods are `async` (they `.await`
    /// downstream MCP calls), and holding a `std::sync::MutexGuard` across
    /// an `.await` point is both a correctness footgun and breaks the
    /// `Send` bound rmcp's tool router needs on the returned future.
    gateway_registry: tokio::sync::Mutex<Option<gateway_registry::Registry>>,
    /// Tests inject a temp path here so they never touch the shared gateway.db.
    gateway_db_override: Option<std::path::PathBuf>,
    /// Handoff identity of the runtime this server instance serves — from
    /// AGENTFLARE_AGENT, baked into the MCP entry by `init --agent <name>`.
    /// Defaults artifact_publish's sender and the handoff prompt's "me".
    agent: Option<String>,
    /// Store + serving backend for `artifact_publish`, resolved lazily on
    /// first use. The default store is served on a shared fixed port that
    /// outlives this process when flared (or an earlier session) owns it;
    /// cross-process live reload works because the SSE handler polls the
    /// disk store.
    artifacts: std::sync::Mutex<
        Option<(
            std::sync::Arc<agentflare_artifacts::ArtifactStore>,
            ArtifactBackend,
        )>,
    >,
    /// Tests inject a temp dir here so they never touch ~/.agentflare/artifacts.
    /// An overridden store is never shared: it always gets its own
    /// OS-assigned port, since the fixed-port server serves the default dir.
    artifacts_dir_override: Option<std::path::PathBuf>,
    /// Lazily-opened connection to agentflare-backend's own database.
    /// Persisted across calls for the same reason as skills_registry: a
    /// fresh connection per call would re-run migrations every time. Unlike
    /// skills (filesystem-derived, needs ensure_fresh), the backend DB is
    /// its own source of truth, so nothing to refresh.
    backend_db: std::sync::Mutex<Option<rusqlite::Connection>>,
    /// Tests inject a temp path here so they never touch the shared backend.db.
    backend_db_override: Option<std::path::PathBuf>,
    /// Tests inject a temp file path here so project-link resolution never
    /// reads/writes this actual repo's `.agentflare/project.json`.
    backend_project_link_override: Option<std::path::PathBuf>,
    /// Tests inject a fake repo identity here — real resolution shells out
    /// to `git`/reads cwd, both process-global and unsafe to fake by
    /// mutating cwd across parallel test threads.
    backend_repo_key_override: Option<String>,
    /// Tests inject a temp repo root here so the worktree-on-claim feature
    /// never runs real git worktree/branch operations against this actual
    /// repository (worktree add, force-remove, branch -D).
    worktree_repo_root_override: Option<std::path::PathBuf>,
}

#[tool_router]
impl AgentflareMcp {
    #[tool(
        description = "Check if a session should be refreshed based on turn count and elapsed time."
    )]
    fn check_session_health(
        &self,
        Parameters(CheckSessionHealthRequest { session_id }): Parameters<CheckSessionHealthRequest>,
    ) -> Result<String, ErrorData> {
        if session_id.is_empty() {
            return Err(ErrorData::invalid_params("session_id is required", None));
        }
        let runtime = optimize::load_runtime();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let result = match runtime.sessions.get(&session_id) {
            Some(record) => match optimize::session_hygiene_nudge(record, now) {
                Some(nudge) => {
                    serde_json::json!({"session_id": session_id, "status": "stale", "nudge": nudge})
                }
                None => serde_json::json!({"session_id": session_id, "status": "healthy"}),
            },
            None => {
                serde_json::json!({"session_id": session_id, "status": "unknown", "message": "Session not tracked"})
            }
        };
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    #[tool(description = "Get a model routing suggestion for a given prompt.")]
    fn get_routing_suggestion(
        &self,
        Parameters(GetRoutingSuggestionRequest { prompt }): Parameters<GetRoutingSuggestionRequest>,
    ) -> String {
        let ctx = optimize::RouteContext {
            prompt,
            session_id: String::new(),
            turn_count: 0,
            recent_tool_calls: vec![],
            current_model: None,
        };
        // Same router the CLI hook uses — honors AGENTFLARE_ROUTER.
        let router = optimize::active_router();
        let result = match router.route(&ctx) {
            Some(nudge) => serde_json::json!({"suggestion": nudge}),
            None => serde_json::json!({"suggestion": null}),
        };
        serde_json::to_string_pretty(&result).unwrap_or_default()
    }

    /// Lock the persisted registry, lazily opening it on first use, refresh
    /// it (debounced inside `Registry::ensure_fresh`), then run `f` against
    /// it. A poisoned lock or an init/refresh failure both map to the same
    /// internal_error the old per-call `open_registry` used.
    fn with_fresh_registry<T>(
        &self,
        f: impl FnOnce(&skill_registry::Registry) -> T,
    ) -> crate::errors::Result<T> {
        let mut guard = self
            .skills_registry
            .lock()
            .map_err(|e| crate::errors::AgentflareError::Lock(e.to_string()))?;
        if guard.is_none() {
            let db_path = self
                .skills_db_override
                .clone()
                .unwrap_or_else(crate::paths::skills_db_path);
            let reg = skill_registry::Registry::open_default(&db_path)?;
            *guard = Some(reg);
        }
        let reg = guard.as_mut().expect("just initialized above");
        reg.ensure_fresh()?;
        Ok(f(reg))
    }
    #[tool(
        description = "Skill operations — search installed skills or load one by name. Single consolidated tool with `action` field (search|load)."
    )]
    async fn skill(&self, Parameters(req): Parameters<SkillRequest>) -> Result<String, ErrorData> {
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
                let local = self
                    .with_fresh_registry(|reg| reg.search(&query, limit, mode))?
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
                    Ok(s) => Ok(serde_json::to_string_pretty(&s).unwrap_or_default()),
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
    /// Filesystem/URL-safe stem derived from a display name — lowercased,
    /// non-alphanumerics collapsed to `-`, falling back to "handoff" if that
    /// leaves nothing.
    fn slugify(name: &str) -> String {
        let s: String = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let trimmed = s.trim_matches('-');
        if trimmed.is_empty() {
            "handoff".to_string()
        } else {
            trimmed.to_string()
        }
    }

    fn content_hash(bytes: &[u8]) -> String {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(bytes);
        hex::encode(&digest[..])
    }

    fn infer_mime_type(ext: &str) -> String {
        match ext.to_lowercase().as_str() {
            "pdf" => "application/pdf".into(),
            "png" => "image/png".into(),
            "jpg" | "jpeg" => "image/jpeg".into(),
            "gif" => "image/gif".into(),
            "svg" => "image/svg+xml".into(),
            "webp" => "image/webp".into(),
            "txt" => "text/plain".into(),
            "md" => "text/markdown".into(),
            "json" => "application/json".into(),
            "csv" => "text/csv".into(),
            "yaml" | "yml" => "application/x-yaml".into(),
            "xml" => "application/xml".into(),
            "html" | "htm" => "text/html".into(),
            "css" => "text/css".into(),
            "js" => "application/javascript".into(),
            "ts" | "tsx" => "application/typescript".into(),
            "rs" => "text/x-rust".into(),
            "py" => "text/x-python".into(),
            "toml" => "application/toml".into(),
            "zip" => "application/zip".into(),
            "tar" => "application/x-tar".into(),
            "gz" => "application/gzip".into(),
            "mp4" => "video/mp4".into(),
            "mp3" => "audio/mpeg".into(),
            "wasm" => "application/wasm".into(),
            _ => "application/octet-stream".into(),
        }
    }

    /// Whether an asset's MIME type denotes text, so its content can be
    /// returned as readable UTF-8 rather than Base64. Binary types are
    /// excluded even when their bytes happen to be valid UTF-8.
    fn mime_is_textual(mime: Option<&str>) -> bool {
        let Some(m) = mime else { return false };
        let m = m.split(';').next().unwrap_or(m).trim();
        m.starts_with("text/")
            || m.ends_with("+json")
            || m.ends_with("+xml")
            || matches!(
                m,
                "application/json"
                    | "application/xml"
                    | "application/javascript"
                    | "application/ecmascript"
                    | "application/typescript"
                    | "application/toml"
                    | "application/x-yaml"
                    | "application/yaml"
            )
    }

    fn asset_max_attach_bytes() -> u64 {
        std::env::var("AGENTFLARE_BACKEND_ASSET_MAX_ATTACH_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5 * 1024 * 1024)
    }

    fn asset_max_inline_bytes() -> u64 {
        std::env::var("AGENTFLARE_BACKEND_ASSET_MAX_INLINE_BYTES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1024 * 1024)
    }

    fn strip_storage_path(asset: &agentflare_backend::asset::Asset) -> serde_json::Value {
        let mut v = serde_json::to_value(asset).unwrap_or_default();
        if let serde_json::Value::Object(ref mut map) = v {
            map.remove("storage_path");
        }
        v
    }

    /// Lock the artifact store + backend pair, resolving the backend on
    /// first use: reuse an already-running artifact server (flared's
    /// /artifacts routes, or another session), else bind the fixed port
    /// ourselves, else fall back to an OS-assigned port. Returns a cloned
    /// store handle and the base URL artifact links hang off, so callers
    /// don't hold the lock while doing I/O.
    fn ensure_artifact_server(
        &self,
    ) -> Result<(std::sync::Arc<agentflare_artifacts::ArtifactStore>, String), ErrorData> {
        let mut guard = self
            .artifacts
            .lock()
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        // An external server can stop at any time (flared restarted, the
        // owning session exited): re-probe before handing out its URL and
        // resolve from scratch when it is gone.
        if let Some((_, ArtifactBackend::External { port, path })) = guard.as_ref()
            && !agentflare_artifacts::probe_path(*port, path)
        {
            *guard = None;
        }
        if guard.is_none() {
            let (store, backend) = match self.artifacts_dir_override.clone() {
                // Overridden stores (tests) stay private: the shared fixed
                // port serves the default store, not this one.
                Some(dir) => {
                    let store = std::sync::Arc::new(agentflare_artifacts::ArtifactStore::new(dir));
                    let server = agentflare_artifacts::ArtifactServer::start(store.clone(), 0)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    (store, ArtifactBackend::Owned(server))
                }
                None => {
                    let dir = crate::paths::home().join(".agentflare").join("artifacts");
                    let store = std::sync::Arc::new(agentflare_artifacts::ArtifactStore::new(dir));
                    let backend = Self::shared_backend(&store)?;
                    (store, backend)
                }
            };
            *guard = Some((store, backend));
        }
        let (store, backend) = guard.as_ref().expect("just initialized above");
        Ok((store.clone(), backend.base_url()))
    }

    /// Resolve serving for the default (shared) store: prefer flared's
    /// always-on /artifacts routes, then another session's fixed-port
    /// server, then bind the fixed port ourselves, else an OS port.
    fn shared_backend(
        store: &std::sync::Arc<agentflare_artifacts::ArtifactStore>,
    ) -> Result<ArtifactBackend, ErrorData> {
        let flared = flared_port();
        if agentflare_artifacts::probe_path(flared, FLARED_ARTIFACTS_PATH) {
            return Ok(ArtifactBackend::External {
                port: flared,
                path: FLARED_ARTIFACTS_PATH,
            });
        }
        let port = agentflare_artifacts::DEFAULT_PORT;
        if agentflare_artifacts::probe(port) {
            return Ok(ArtifactBackend::External { port, path: "/" });
        }
        match agentflare_artifacts::ArtifactServer::start(store.clone(), port) {
            Ok(server) => Ok(ArtifactBackend::Owned(server)),
            // Lost the bind race to another session starting up, or a
            // foreign app owns the port.
            Err(_) if agentflare_artifacts::probe(port) => {
                Ok(ArtifactBackend::External { port, path: "/" })
            }
            Err(_) => agentflare_artifacts::ArtifactServer::start(store.clone(), 0)
                .map(ArtifactBackend::Owned)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None)),
        }
    }
    #[tool(
        description = "Artifact operations — publish, list, get, diff, search, or delete. Single consolidated tool with `action` field (delete|diff|get|list|publish|search)."
    )]
    fn artifact(&self, Parameters(req): Parameters<ArtifactRequest>) -> Result<String, ErrorData> {
        self.artifact_impl(req)
    }
    #[tool(
        description = "Hand a work product to another agent: assigns/creates an item for the recipient (in the repo's linked project) and attaches the content to it as an asset. Pass `item_id` to target an existing item instead of creating a new one — omitting it always mints a new item, so if the work already has a home item, pass its id. Re-attaching under the same item_id creates the next asset version, not a duplicate. For a plain-text status update with no versioned artifact to attach, prefer `comment` (action=create) + `item` (action=update, id=<id>, assignee_agent=...) instead of this tool — lighter, no new item, no asset. Sender is this runtime's own identity."
    )]
    fn handoff(&self, Parameters(req): Parameters<HandoffRequest>) -> Result<String, ErrorData> {
        self.handoff_impl(req)
    }

    /// Runs `git` in the current cwd; None on any failure (not a repo, git
    /// not on PATH, etc). Shared by `git_provenance` and the backend
    /// project-link resolution below.
    fn run_git(args: &[&str]) -> Option<String> {
        crate::git::run_in_opt(&std::env::current_dir().unwrap_or_default(), args)
    }

    /// Best-effort git context of this process's cwd (the project the MCP
    /// server was launched in). None outside a repo; never fails a publish.
    pub(crate) fn git_provenance() -> Option<agentflare_artifacts::GitProvenance> {
        let commit = Self::run_git(&["rev-parse", "HEAD"])?;
        Some(agentflare_artifacts::GitProvenance {
            repo: Self::run_git(&["remote", "get-url", "origin"]),
            r#ref: Self::run_git(&["rev-parse", "--abbrev-ref", "HEAD"]),
            commit: Some(commit),
        })
    }

    /// Directory the per-repo project link (`project.json`) lives under.
    /// Same name as this codebase's global per-user data dir
    /// (`crate::paths::home().join(".agentflare")`, holding `agentflare.db`,
    /// artifacts, etc.) — that's fine ONLY because `find_root_from`'s
    /// walk-up is hard-bounded to never reach the user's home directory
    /// (see below); the global dir only ever exists at exactly that one
    /// path, so excluding that one path from the walk is sufficient to
    /// keep the two `.agentflare` folders — the global one at `~/` and any
    /// number of per-repo ones elsewhere — from ever being confused for
    /// each other.
    ///
    /// Once a project is linked here, every subdirectory below it must keep
    /// resolving to that same project — checked as its own walk-up pass,
    /// ahead of `ROOT_MARKERS`, so a nested subdirectory's own marker (e.g.
    /// a monorepo package's own `package.json`) never shadows an
    /// already-linked ancestor.
    const LINK_MARKER: &'static str = ".agentflare";

    /// Fallback markers for a non-git project with no existing link yet —
    /// mirrors what `git rev-parse --show-toplevel` already gives git repos
    /// for free. Nearest ancestor (including the start dir) with any of
    /// these wins.
    const ROOT_MARKERS: &'static [&'static str] = &[
        ".git",
        "package.json",
        "pyproject.toml",
        "go.mod",
        ".hg",
        ".svn",
    ];

    /// Repo root for git projects (`git rev-parse --show-toplevel` — handles
    /// worktrees/submodules correctly, works regardless of subdirectory).
    /// For non-git projects, walks up from cwd looking for `LINK_MARKER`
    /// then `ROOT_MARKERS` the same way git itself walks up looking for
    /// `.git` — without this, a non-git project's root would be "whatever
    /// cwd happened to be at call time," splitting one logical project
    /// across multiple linked projects depending on which subdirectory a
    /// tool was invoked from. Falls back to raw cwd only when nothing is
    /// found anywhere above it.
    pub(crate) fn repo_root() -> std::path::PathBuf {
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(root) = crate::git::repo_toplevel(&cwd) {
            return root;
        }
        Self::find_root_from(&cwd, &crate::paths::home())
    }

    /// `repo_root()`, but honoring `worktree_repo_root_override` — used only
    /// by the worktree-on-claim feature so tests never run real `git
    /// worktree`/branch operations against this actual repository.
    pub(crate) fn worktree_repo_root(&self) -> std::path::PathBuf {
        self.worktree_repo_root_override
            .clone()
            .unwrap_or_else(Self::repo_root)
    }

    /// Test-only constructor: an isolated instance backed entirely by the
    /// given paths, so tests outside this module (e.g. `cli::work`'s
    /// integration tests) never touch the shared `backend.db`,
    /// `project.json`, or run real `git worktree` operations against the
    /// actual repo `cargo test` is running in.
    #[cfg(test)]
    pub(crate) fn for_test(
        backend_db: std::path::PathBuf,
        worktree_repo_root: std::path::PathBuf,
        project_link: std::path::PathBuf,
    ) -> Self {
        Self {
            backend_db_override: Some(backend_db),
            backend_project_link_override: Some(project_link),
            worktree_repo_root_override: Some(worktree_repo_root),
            ..Default::default()
        }
    }

    /// Pure walk-up so the non-git fallback path is unit-testable without
    /// touching process-global state: neither this process's real cwd nor
    /// `crate::paths::home()` (which itself reads the `AGENTFLARE_HOME_OVERRIDE`
    /// env var other tests mutate concurrently under their own lock — calling
    /// it from here directly made this function's result depend on unrelated
    /// tests' timing). Both are passed in by the caller instead. Never walks
    /// as far as (or past) `home` — this is what keeps `LINK_MARKER`
    /// (`.agentflare`, same name as the global per-user data dir at
    /// `~/.agentflare`) safe to reuse: the walk simply never gets far enough
    /// up to see that directory at all, regardless of what marker name it's
    /// looking for.
    fn find_root_from(start: &std::path::Path, home: &std::path::Path) -> std::path::PathBuf {
        let mut dir = start;
        while dir != home {
            if dir.join(Self::LINK_MARKER).exists() {
                return dir.to_path_buf();
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
        let mut dir = start;
        while dir != home {
            if Self::ROOT_MARKERS.iter().any(|m| dir.join(m).exists()) {
                return dir.to_path_buf();
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => return start.to_path_buf(),
            }
        }
        start.to_path_buf()
    }

    fn project_link_path(&self) -> std::path::PathBuf {
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
    fn resolve_project_name() -> String {
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

    /// Lock the backend connection, lazily opening it (and running its
    /// migrations) on first use, then run `f` against it. The backend DB is
    /// its own source of truth — no filesystem-derived refresh needed,
    /// unlike `with_fresh_registry` above.
    pub(crate) fn with_backend_db<T>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> T,
    ) -> Result<T, ErrorData> {
        let mut guard = self
            .backend_db
            .lock()
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        if guard.is_none() {
            let db_path = self
                .backend_db_override
                .clone()
                .unwrap_or_else(|| crate::paths::home().join(".agentflare").join("backend.db"));
            let conn = agentflare_backend::db::open_db(&db_path)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            *guard = Some(conn);
        }
        Ok(f(guard.as_ref().expect("just initialized above")))
    }

    /// The one and only workspace on this system: reused if it already
    /// exists, auto-created (named "default") on first use. Never exposed
    /// as an MCP parameter.
    fn resolve_workspace_id(conn: &rusqlite::Connection) -> crate::errors::Result<String> {
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
        let canonical = std::fs::canonicalize(&root).unwrap_or(root);
        format!("path:{}", canonical.to_string_lossy())
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
                Ok(project) => return Ok(project),
                Err(agentflare_backend::Error::NotFound(_)) => {} // stale link — re-resolve below
                Err(e) => return Err(map_backend_err(e)),
            }
        }

        let workspace_id = Self::resolve_workspace_id(conn)?;
        let name = Self::resolve_project_name();
        let identifier = Self::derive_project_identifier(&name);
        let repo_key = self.resolve_repo_key();

        let existing = agentflare_backend::project::list_by_workspace(conn, &workspace_id)
            .map_err(map_backend_err)?
            .into_iter()
            .find(|p| {
                p.external_source.as_deref() == Some(Self::REPO_EXTERNAL_SOURCE)
                    && p.external_id.as_deref() == Some(repo_key.as_str())
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
        Ok(project)
    }

    /// NotFound and InvalidInput (version conflict) are caller-fixable →
    /// invalid_params; everything else is an internal error.
    fn artifact_error(e: std::io::Error) -> ErrorData {
        match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput => {
                ErrorData::invalid_params(e.to_string(), None)
            }
            _ => ErrorData::internal_error(e.to_string(), None),
        }
    }

    #[tool(
        description = "Send a text message out to a chat platform (telegram, slack, or discord). The bot token must already be stored as the gateway secret '<platform>_bot_token'. target is the Telegram chat_id or Slack/Discord channel id."
    )]
    fn channel_send(
        &self,
        Parameters(ChannelSendRequest {
            platform,
            target,
            message,
        }): Parameters<ChannelSendRequest>,
    ) -> Result<String, ErrorData> {
        let plat = crate::channels::Platform::parse(&platform).ok_or_else(|| {
            ErrorData::invalid_params(
                format!("unknown platform '{platform}' (expected telegram, slack, or discord)"),
                None,
            )
        })?;
        let conn = crate::db::open()
            .map_err(|e| ErrorData::internal_error(format!("cannot open database: {e}"), None))?;
        crate::channels::send_message(&conn, plat, &target, &message)
            .map_err(|e| ErrorData::internal_error(e, None))?;
        Ok(serde_json::json!({ "sent": true, "platform": platform, "target": target }).to_string())
    }
    #[tool(
        description = "Manage work claims — acquire, heartbeat, release, done, or list. Single consolidated tool with `action` field (acquire|done|heartbeat|list|release)."
    )]
    fn claim(&self, Parameters(req): Parameters<ClaimRequest>) -> Result<String, ErrorData> {
        self.claim_impl(req)
    }
    /// Opens the ledger db.
    fn claim_db() -> crate::errors::Result<rusqlite::Connection> {
        Ok(crate::db::open()?)
    }

    /// Shared prelude for the per-target claim tools: validate target, open the
    /// ledger, resolve the repo key.
    fn claim_ctx(
        target: &str,
        repo: Option<String>,
    ) -> Result<(rusqlite::Connection, String), ErrorData> {
        if target.trim().is_empty() {
            return Err(ErrorData::invalid_params("target is required", None));
        }
        let conn = Self::claim_db()?;
        let repo = crate::claims::resolve_repo(repo).ok_or_else(|| {
            ErrorData::invalid_params(
                "could not determine repo — run in a git repo or pass repo=owner/name",
                None,
            )
        })?;
        Ok((conn, repo))
    }
    #[tool(
        description = "Review operations — submit findings, run consensus, list/clear/record rounds, check scores. Single consolidated tool with `action` field (clear|consensus|list|record|scores|submit)."
    )]
    fn review(&self, Parameters(req): Parameters<ReviewRequest>) -> Result<String, ErrorData> {
        self.review_impl(req)
    }
    fn resolve_repo_or_err(repo: Option<String>) -> Result<String, ErrorData> {
        crate::claims::resolve_repo(repo).ok_or_else(|| {
            ErrorData::invalid_params(
                "could not determine repo — run in a git repo or pass repo=owner/name",
                None,
            )
        })
    }

    /// Review round id: explicit `pr`, else the current branch name.
    fn resolve_round(pr: Option<String>) -> Result<String, ErrorData> {
        if let Some(pr) = pr.filter(|s| !s.is_empty()) {
            return Ok(pr);
        }
        crate::git::current_branch(&std::env::current_dir().unwrap_or_default())
            .ok_or_else(|| ErrorData::invalid_params("could not determine round — pass pr", None))
    }

    fn gateway_db_path() -> std::path::PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("agentflare")
            .join("gateway.db")
    }

    fn load_gateway_config() -> gateway_registry::GatewayConfig {
        let path = crate::paths::home()
            .join(".agentflare")
            .join("gateway.toml");
        match std::fs::read_to_string(&path) {
            Ok(s) => match gateway_registry::parse_config(&s) {
                Ok(config) => config,
                Err(e) => {
                    // Malformed TOML, or a config that fails the
                    // auth_ref/auth_env pairing check, used to look
                    // identical to "no gateway.toml yet" — both silently
                    // produced an empty config with zero configured
                    // servers. Surface the parse error so a user who
                    // typo'd their gateway.toml gets visible signal instead
                    // of a silent "no servers configured".
                    eprintln!(
                        "agentflare: failed to parse gateway config at {}: {e} — using no configured servers",
                        path.display()
                    );
                    gateway_registry::GatewayConfig::default()
                }
            },
            // The file genuinely doesn't exist yet (or isn't readable) —
            // the normal, expected case for a user who hasn't set up
            // gateway.toml. No log needed here.
            Err(_) => gateway_registry::GatewayConfig::default(),
        }
    }

    fn resolve_gateway_secrets() -> std::collections::HashMap<String, String> {
        let conn = match crate::db::open() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("agentflare: failed to open agentflare.db for gateway secrets: {e}");
                return std::collections::HashMap::new();
            }
        };
        let names = match crate::gateway_secrets::list_secrets(&conn) {
            Ok(names) => names,
            Err(e) => {
                eprintln!("agentflare: failed to list gateway secrets: {e}");
                return std::collections::HashMap::new();
            }
        };
        names
            .into_iter()
            .filter_map(
                |name| match crate::gateway_secrets::get_secret(&conn, &name) {
                    Ok(Some(v)) => Some((name, v)),
                    Ok(None) => None,
                    Err(e) => {
                        // A wrong/missing vault passphrase used to look
                        // identical to "no secret configured" — `.ok().flatten()`
                        // discarded the `Err` entirely. Surface it so a wrong
                        // passphrase is at least visible in stderr instead of
                        // silently leaving downstream backends uncredentialed.
                        eprintln!("agentflare: failed to resolve gateway secret '{name}': {e}");
                        None
                    }
                },
            )
            .collect()
    }

    /// Ensures `self.gateway_registry` holds an initialized, freshly-
    /// refreshed `Registry`, then returns the still-locked guard so the
    /// caller can use it directly. Safe to hold across further `.await`
    /// points on the returned guard — `tokio::sync::MutexGuard` (unlike
    /// `std::sync::MutexGuard`) is designed for exactly that, which is why
    /// `gateway_registry` is a `tokio::sync::Mutex` and `skills_registry`
    /// isn't. (An earlier draft tried to fold `Registry::execute` — an
    /// async fn — into a plain `FnOnce(&Registry) -> T` callback shared
    /// with the "search" action arm; that doesn't compile without unstable
    /// async-closure/HRTB machinery, so each tool method just calls this
    /// helper and then works with the guard itself.)
    async fn ensure_gateway_registry(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<gateway_registry::Registry>>, ErrorData> {
        let mut guard = self.gateway_registry.lock().await;
        if guard.is_none() {
            let db_path = self
                .gateway_db_override
                .clone()
                .unwrap_or_else(Self::gateway_db_path);
            let config = Self::load_gateway_config();
            let secrets = Self::resolve_gateway_secrets();
            let reg = gateway_registry::Registry::open_default(&db_path, &config, &secrets)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            *guard = Some(reg);
        }
        guard
            .as_mut()
            .expect("just initialized above")
            .ensure_fresh()
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(guard)
    }
    #[tool(
        description = "Tool operations — search downstream MCP servers' tools by task description or execute one. Single consolidated tool with `action` field (search|execute)."
    )]
    async fn tool(&self, Parameters(req): Parameters<ToolRequest>) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "search" => {
                let query = req
                    .query
                    .ok_or_else(|| ErrorData::invalid_params("query is required", None))?;
                if query.trim().is_empty() {
                    return Err(ErrorData::invalid_params("query is required", None));
                }
                let mode = match req.mode.as_deref() {
                    None | Some("all") => gateway_registry::MatchMode::All,
                    Some("any") => gateway_registry::MatchMode::Any,
                    Some(other) => {
                        return Err(ErrorData::invalid_params(
                            format!("mode must be 'all' or 'any', got '{other}'"),
                            None,
                        ));
                    }
                };
                let limit = req.limit.unwrap_or(5);
                let local = {
                    let guard = self.ensure_gateway_registry().await?;
                    let reg = guard.as_ref().expect("ensured above");
                    reg.search(&query, limit, mode)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                };
                let hits = if local.len() < limit {
                    let remaining = limit - local.len();
                    let query_owned = query.clone();
                    let registry = tokio::task::spawn_blocking(move || {
                        gateway_registry::registry_search::search_registry(&query_owned, remaining)
                    })
                    .await
                    .unwrap_or_default();
                    gateway_registry::merge_registry_hits(local, limit, registry)
                } else {
                    local
                };
                Ok(serde_json::to_string_pretty(&hits).unwrap_or_default())
            }
            "execute" => {
                let server = req
                    .server
                    .ok_or_else(|| ErrorData::invalid_params("server is required", None))?;
                let tool = req
                    .tool
                    .ok_or_else(|| ErrorData::invalid_params("tool is required", None))?;
                if server.trim().is_empty() || tool.trim().is_empty() {
                    return Err(ErrorData::invalid_params(
                        "server and tool are required",
                        None,
                    ));
                }
                let args = req
                    .args
                    .map(serde_json::Value::Object)
                    .unwrap_or(serde_json::Value::Null);
                let guard = self.ensure_gateway_registry().await?;
                let reg = guard.as_ref().expect("ensured above");
                match reg.execute(&server, &tool, args).await {
                    Ok(value) => {
                        let capped = gateway_registry::truncate_if_needed(
                            &value,
                            gateway_registry::DEFAULT_MAX_CHARS,
                        );
                        Ok(serde_json::to_string_pretty(&capped).unwrap_or_default())
                    }
                    Err(e @ gateway_registry::GatewayError::ServerNotFound(_))
                    | Err(e @ gateway_registry::GatewayError::ToolNotFound(_))
                    | Err(e @ gateway_registry::GatewayError::InvalidArgument(_)) => {
                        Err(ErrorData::invalid_params(e.to_string(), None))
                    }
                    Err(e) => Err(ErrorData::internal_error(
                        gateway_registry::redact_error_for_llm(&e.to_string()),
                        None,
                    )),
                }
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    } // --- Memory tools ---
    #[tool(
        description = "Memory operations — compact, context, curate, handoff, recall, relate, or remember observations. Single consolidated tool with `action` field (compact|context|curate|handoff|recall|relate|remember)."
    )]
    fn memory(&self, Parameters(req): Parameters<MemoryRequest>) -> Result<String, ErrorData> {
        self.memory_impl(req)
    }

    #[tool(
        description = "Vent friction when the TOOLING blocks you (not the task) — a wrong/missing tool, a fabricated assumption, an environment gap. Actionable vents auto-file a DX item once per turn; noise is just logged. Use sparingly, exactly when you're genuinely blocked. DO: \"The $CLAUDE_JOB_DIR I assumed exists is empty — I fabricated it; there's no such env var and my temp writes went to /.\" DON'T: \"This build is slow to compile.\" Inputs: message (required), severity (low|medium|high), tags."
    )]
    fn vent(&self, Parameters(req): Parameters<VentRequest>) -> Result<String, ErrorData> {
        if req.message.trim().is_empty() {
            return Err(ErrorData::invalid_params("message is required", None));
        }
        let severity = crate::vent::classify::normalize_severity(req.severity.as_deref());
        let tags = req.tags.unwrap_or_default();
        let log = crate::vent::paths::log_path();
        let event_id =
            crate::vent::capture::append(&log, None, severity, &tags, req.message.trim()).map_err(
                |e| ErrorData::internal_error(format!("vent capture failed: {e}"), None),
            )?;
        Ok(serde_json::json!({ "ok": true, "event_id": event_id }).to_string())
    }

    /// Rejects PR titles that don't start with a conventional-commit type,
    /// mirroring `.github/workflows/pr-title.yml`'s
    /// `amannn/action-semantic-pull-request` config so the check fires here
    /// instead of only after push+PR-open. Keep this type list in sync with
    /// that workflow file if it changes.
    fn validate_conventional_pr_title(title: &str) -> Result<(), String> {
        const TYPES: &[&str] = &[
            "feat", "fix", "docs", "perf", "refactor", "style", "test", "chore", "ci",
        ];
        let pattern = format!(r"^(?:{})(?:\([^)]+\))?!?:\s", TYPES.join("|"));
        let re = regex::Regex::new(&pattern).expect("valid conventional-commit regex");
        if re.is_match(title) {
            Ok(())
        } else {
            Err(format!(
                "PR title must start with a conventional-commit type ({}), e.g. \"chore: ...\" — got {title:?}",
                TYPES.join(", ")
            ))
        }
    }
    #[tool(
        description = "GitHub repo management via the flare_git module. Single action-dispatch tool: action=pr_create|pr_list|pr_get|pr_status|pr_merge|pr_comment|pr_request_review|issue_create|issue_list|issue_get|issue_comment|issue_close|issue_label|release_list|release_get|release_latest|release_create|run_list|run_get|run_rerun|workflow_dispatch. pr_status bundles PR detail + CI checks + reviews + comments into one call (vs. 4-5 separate ones), trimmed to only actionable data: passing checks are counted not listed, resolved review threads are dropped, only the latest verdict per reviewer is kept, and there are no timestamps. Pass `since` (ISO8601) to fetch only newer comments. Uses gh/GITHUB_TOKEN credentials; repo defaults to the current repo's origin."
    )]
    fn flare_git(&self, Parameters(req): Parameters<GitHubRequest>) -> Result<String, ErrorData> {
        self.flare_git_impl(req)
    }
    #[tool(
        description = "Optimize layer — reversible-compression retrieval (CCR). action=retrieve returns the original for a registered id; action=list enumerates live entries."
    )]
    fn optimize(&self, Parameters(req): Parameters<OptimizeRequest>) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "retrieve" => {
                let id = req.id.ok_or_else(|| {
                    ErrorData::invalid_params("id is required for retrieve", None)
                })?;
                crate::optimize::retrieve::retrieve(&id)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))
            }
            "list" => {
                let state =
                    crate::optimize::retrieve::active_state(crate::optimize::retrieve::now_unix());
                let mut entries: Vec<_> = state.entries.values().collect();
                entries.sort_by_key(|e| std::cmp::Reverse(e.created_ts));
                let summary: Vec<_> = entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "id": e.id,
                            "kind": crate::optimize::retrieve::kind_label(&e.kind),
                            "size_before": e.size_before,
                            "size_after": e.size_after,
                            "created_ts": e.created_ts,
                        })
                    })
                    .collect();
                serde_json::to_string(&summary)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }
    fn item_inner(&self, req: ItemRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "create" => self.item_create(req),
            "get" => self.item_get(req),
            "list" => self.item_list(req),
            "update" => self.item_update(req),
            "update_state" => self.item_update_state(req),
            "delete" => self.item_delete(req),
            "claim" => self.item_claim(req),
            "heartbeat" => self.item_heartbeat(req),
            "release" => self.item_release(req),
            "done" => self.item_done(req),
            "cancel" => self.item_cancel(req),
            "search" => self.item_search(req),
            "add_label" => self.item_add_label(req),
            "remove_label" => self.item_remove_label(req),
            "groom" => self.item_groom(req),
            "standup" => self.item_standup(req),
            "health" => self.item_health(req),
            other => Err(ErrorData::invalid_params(
                format!(
                    "unknown item action: '{other}' — expected create|get|list|search|update|update_state|delete|claim|heartbeat|release|done|cancel|add_label|remove_label|groom|standup|health"
                ),
                None,
            )),
        }
    }

    #[tool(
        description = "Manage work items in the repo's linked project. Single consolidated tool with `action` field (create|get|list|search|update|update_state|delete|claim|heartbeat|release|done|cancel|add_label|remove_label|groom|standup|health). `groom` returns a priority+staleness-ranked shortlist with description, stale/unassigned/blocked/duplicate flags, and a pull_next list — all in one call, no per-item `get` round trips needed. `standup` returns done/in_progress(grouped by assignee)/stuck buckets computed server-side. `health` returns a velocity/WIP/stuck scorecard; `bottlenecks` is currently always empty — no handoff log is persisted yet, see `bottleneck_note`. See each field's description for when it's required."
    )]
    fn item(&self, Parameters(req): Parameters<ItemRequest>) -> Result<String, ErrorData> {
        self.item_inner(req)
    }

    #[tool(
        description = "Create, edit, delete, or list threaded comments on an item. Single consolidated tool with `action` field (create|edit|delete|list). Only the author of a comment may edit/delete it, only the latest comment on an item is editable/deletable, and edit/delete are blocked while another agent holds an active claim on the item."
    )]
    fn comment(&self, Parameters(req): Parameters<CommentRequest>) -> Result<String, ErrorData> {
        self.comment_impl(req)
    }

    /// Verify a label belongs to the repo's resolved project before mutating it by
    /// ID, so `update`/`delete` can't reach across projects by guessing an ID.
    /// Returns `invalid_params` on mismatch (same shape as not-found).
    fn ensure_label_in_project(
        &self,
        conn: &rusqlite::Connection,
        label_id: &str,
    ) -> Result<(), ErrorData> {
        let project = self.resolve_project(conn)?;
        let label = agentflare_backend::label::get(conn, label_id).map_err(map_backend_err)?;
        if label.project_id.as_deref() != Some(project.id.as_str()) {
            return Err(ErrorData::invalid_params(
                format!("label {label_id} is not in this project"),
                None,
            ));
        }
        Ok(())
    }

    fn label_inner(&self, req: LabelRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "create" => {
                let name = req.name.ok_or_else(|| {
                    ErrorData::invalid_params("name is required for create", None)
                })?;
                if name.trim().is_empty() {
                    return Err(ErrorData::invalid_params("name is required", None));
                }
                self.with_backend_db(|conn| {
                    let project = self.resolve_project(conn)?;
                    let input = agentflare_backend::label::CreateLabel {
                        project_id: Some(project.id.clone()),
                        workspace_id: project.workspace_id,
                        name,
                        color: req.color,
                        parent_id: req.parent_id,
                        sort_order: req.sort_order,
                        external_source: None,
                        external_id: None,
                    };
                    let label =
                        agentflare_backend::label::create(conn, input).map_err(map_backend_err)?;
                    Ok(serde_json::to_string_pretty(&label).unwrap_or_default())
                })?
            }
            "list" => self.with_backend_db(|conn| {
                let project = self.resolve_project(conn)?;
                let labels = agentflare_backend::label::list_by_project(conn, &project.id)
                    .map_err(map_backend_err)?;
                Ok(serde_json::to_string_pretty(&labels).unwrap_or_default())
            })?,
            "update" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required for update", None))?;
                let input = agentflare_backend::label::UpdateLabel {
                    name: req.name,
                    color: req.color,
                    sort_order: req.sort_order,
                };
                self.with_backend_db(|conn| {
                    self.ensure_label_in_project(conn, &id)?;
                    let label = agentflare_backend::label::update(conn, &id, input)
                        .map_err(map_backend_err)?;
                    Ok(serde_json::to_string_pretty(&label).unwrap_or_default())
                })?
            }
            "delete" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required for delete", None))?;
                self.with_backend_db(|conn| {
                    self.ensure_label_in_project(conn, &id)?;
                    agentflare_backend::label::delete(conn, &id).map_err(map_backend_err)?;
                    Ok(serde_json::json!({"deleted": true, "id": id}).to_string())
                })?
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown label action: '{other}' — expected create|list|update|delete"),
                None,
            )),
        }
    }

    #[tool(
        description = "Manage labels in the repo's linked project. The `action` field selects the operation (create|list|update|delete)."
    )]
    fn label(&self, Parameters(req): Parameters<LabelRequest>) -> Result<String, ErrorData> {
        self.label_inner(req)
    }

    fn webhook_inner(&self, req: WebhookRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "create" => {
                let url = req
                    .url
                    .ok_or_else(|| ErrorData::invalid_params("url is required for create", None))?;
                if url.trim().is_empty() {
                    return Err(ErrorData::invalid_params("url is required", None));
                }
                self.with_backend_db(|conn| {
                    let project = self.resolve_project(conn)?;
                    let secret_key = req.secret.unwrap_or_else(generate_webhook_secret);
                    let input = agentflare_backend::webhook::CreateWebhook {
                        workspace_id: project.workspace_id,
                        url,
                        secret_key,
                        on_item: req.on_item,
                        on_state: req.on_state,
                        on_project: req.on_project,
                    };
                    let webhook = agentflare_backend::webhook::create(conn, input)
                        .map_err(map_backend_err)?;
                    let mut value = serde_json::to_value(&webhook).unwrap_or_default();
                    if let Some(obj) = value.as_object_mut() {
                        obj.insert(
                            "secret_key".to_string(),
                            serde_json::Value::String(webhook.secret_key.clone()),
                        );
                    }
                    Ok(serde_json::to_string_pretty(&value).unwrap_or_default())
                })?
            }
            "list" => self.with_backend_db(|conn| {
                let project = self.resolve_project(conn)?;
                let webhooks =
                    agentflare_backend::webhook::list_by_workspace(conn, &project.workspace_id)
                        .map_err(map_backend_err)?;
                Ok(serde_json::to_string_pretty(&webhooks).unwrap_or_default())
            })?,
            "delete" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required for delete", None))?;
                if id.trim().is_empty() {
                    return Err(ErrorData::invalid_params("id is required", None));
                }
                self.with_backend_db(|conn| {
                    agentflare_backend::webhook::delete(conn, &id).map_err(map_backend_err)?;
                    Ok(serde_json::json!({"deleted": true, "id": id}).to_string())
                })?
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown webhook action: '{other}' — expected create|list|delete"),
                None,
            )),
        }
    }

    #[tool(
        description = "Register, list, or delete webhooks on the repo's linked workspace. The `action` field selects the operation. secret is auto-generated if omitted for create — save the returned value, it isn't shown again."
    )]
    fn webhook(&self, Parameters(req): Parameters<WebhookRequest>) -> Result<String, ErrorData> {
        self.webhook_inner(req)
    }

    fn project_inner(&self, req: ProjectRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "info" => self.with_backend_db(|conn| {
                let project = self.resolve_project(conn)?;
                Ok(serde_json::to_string_pretty(&project).unwrap_or_default())
            })?,
            other => Err(ErrorData::invalid_params(
                format!("unknown project action: '{other}' — expected info"),
                None,
            )),
        }
    }

    #[tool(
        description = "Show the workspace/project this repo is currently linked to (auto-created/linked on first use). The `action` field selects the operation (only `info` for now)."
    )]
    fn project(&self, Parameters(req): Parameters<ProjectRequest>) -> Result<String, ErrorData> {
        self.project_inner(req)
    }

    #[tool(
        description = "Attach, get, list, or delete file assets on items/projects. Attach requires the file to exist in ~/.agentflare/staging/<filename> first."
    )]
    fn asset(&self, Parameters(req): Parameters<AssetRequest>) -> Result<String, ErrorData> {
        self.asset_impl(req)
    }
}

impl AgentflareMcp {
    /// Pure logic backing [`ServerHandler::list_resources`]; kept as a plain
    /// sync method so it can be unit-tested without constructing a
    /// `RequestContext<RoleServer>`.
    fn list_resources_sync(&self) -> ListResourcesResult {
        let runtime = optimize::load_runtime();
        let sessions_resource = RawResource {
            description: Some(format!("{} tracked sessions", runtime.sessions.len())),
            mime_type: Some("application/json".to_string()),
            ..RawResource::new("agentflare://sessions", "Active sessions")
        };
        let nudges_resource = RawResource {
            description: Some("All nudge types agentflare can emit".to_string()),
            mime_type: Some("application/json".to_string()),
            ..RawResource::new("agentflare://nudges", "Optimization nudges")
        };
        ListResourcesResult::with_all_items(vec![
            sessions_resource.no_annotation(),
            nudges_resource.no_annotation(),
        ])
    }

    /// Pure logic backing [`ServerHandler::read_resource`]; kept as a plain
    /// sync method so it can be unit-tested without constructing a
    /// `RequestContext<RoleServer>`.
    fn read_resource_sync(&self, uri: &str) -> Result<ReadResourceResult, ErrorData> {
        let text = match uri {
            "agentflare://sessions" => {
                let runtime = optimize::load_runtime();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let sessions: Vec<serde_json::Value> = runtime
                    .sessions
                    .iter()
                    .map(|(id, record)| {
                        let elapsed_secs = now.saturating_sub(record.start_ts);
                        let hygiene = optimize::session_hygiene_nudge(record, now);
                        serde_json::json!({
                            "session_id": id,
                            "turn_count": record.turn_count,
                            "age_seconds": elapsed_secs,
                            "age_hours": elapsed_secs / 3600,
                            "recent_tool_calls": record.recent_tool_calls.iter().map(|c| serde_json::json!({
                                "name": c.name,
                                "ts": c.ts,
                            })).collect::<Vec<_>>(),
                            "hygiene_status": if hygiene.is_some() { "stale" } else { "healthy" },
                            "hygiene_nudge": hygiene,
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&sessions).unwrap_or_default()
            }
            "agentflare://nudges" => serde_json::to_string_pretty(&serde_json::json!({
                "nudges": [
                    {
                        "id": "session_hygiene",
                        "description": "Warns when a session exceeds turn/time thresholds",
                        "thresholds": {
                            "turns": optimize::SESSION_HYGIENE_TURN_THRESHOLD,
                            "time_seconds": optimize::SESSION_HYGIENE_TIME_THRESHOLD_SECS
                        }
                    },
                    {
                        "id": "model_routing",
                        "description": "Suggests cheap models for locate/investigate tasks"
                    },
                    {
                        "id": "batching",
                        "description": "Flags repeated solo tool calls that should be batched"
                    },
                    {
                        "id": "schedule_wakeup",
                        "description": "Warns about cache-miss dead zone in scheduling delays"
                    }
                ]
            }))
            .unwrap_or_default(),
            _ => {
                return Err(ErrorData::resource_not_found(
                    format!("Unknown resource: {uri}"),
                    None,
                ));
            }
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            text, uri,
        )]))
    }
}

/// Central hint table: (tool_name, result_json) → optional `next` hint.
/// Post-processed after every tool call so no individual tool action needs
/// to remember to inject a hint inline.
fn next_hint(tool_name: &str, json: &serde_json::Value) -> Option<String> {
    let obj = json.as_object()?;
    match tool_name {
        "item" => {
            if obj.contains_key("worktree_path") {
                Some("cd into worktree_path — do all work for this item there".into())
            } else {
                obj.get("pr_url").and_then(|v| v.as_str()).map(|url| {
                    format!(
                        "PR opened at {url} — wait for review/merge before removing the worktree"
                    )
                })
            }
        }
        "handoff" => Some("Run /handoff inbox to check for replies".into()),
        _ => None,
    }
}

#[tool_handler]
impl ServerHandler for AgentflareMcp {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let tool_name = request.name.to_string();
        let progress_token = request.meta.as_ref().and_then(Meta::get_progress_token);
        let progress_sender =
            progress_token.map(|token| ProgressSender::new(context.peer.clone(), token));
        let tcc = ToolCallContext::new(self, request, context);
        let mut result = PROGRESS_SENDER
            .scope(progress_sender, Self::tool_router().call(tcc))
            .await?;
        let mut content_json =
            serde_json::to_value(&result.content).unwrap_or(serde_json::Value::Null);
        if let Some(arr) = content_json.as_array_mut()
            && let Some(first) = arr.first_mut()
            && let Some(text) = first.get("text").and_then(|v| v.as_str())
            && let Ok(mut json) = serde_json::from_str::<serde_json::Value>(text)
            && let Some(hint) = next_hint(&tool_name, &json)
            && let serde_json::Value::Object(ref mut map) = json
        {
            map.insert("next".into(), serde_json::Value::String(hint));
            let new_text = serde_json::to_string_pretty(&map).unwrap_or_else(|_| text.into());
            first["text"] = serde_json::Value::String(new_text);
            result.content = serde_json::from_value(content_json).unwrap_or(result.content);
        }
        Ok(result)
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_prompts()
                .build(),
        )
        .with_server_info(Implementation::new(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(self.list_resources_sync())
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        self.read_resource_sync(request.uri.as_str())
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(
            crate::mcp_prompts::list_prompts(),
        ))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        crate::mcp_prompts::get_prompt(&request, self.agent.as_deref()).ok_or_else(|| {
            ErrorData::invalid_params(format!("Unknown prompt: {}", request.name), None)
        })
    }
}

impl AgentflareMcp {
    /// Runtime identity: explicit override wins, else auto-detect the host
    /// that launched us (parent process walk + agent env fingerprints).
    fn identity(explicit: Option<String>) -> Option<String> {
        explicit
            .filter(|s| !s.is_empty())
            .or_else(agent_detector::agent_name)
    }

    /// Production constructor: identity from AGENTFLARE_AGENT or detection.
    fn from_env() -> Self {
        AgentflareMcp {
            agent: Self::identity(std::env::var("AGENTFLARE_AGENT").ok()),
            ..Default::default()
        }
    }
}

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = AgentflareMcp::from_env().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests;
