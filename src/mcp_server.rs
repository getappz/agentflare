//! MCP (Model Context Protocol) server over stdio, built on the `rmcp` crate
//! (`modelcontextprotocol/rust-sdk`, published to crates.io — a normal
//! dependency, not ported code; no /NOTICE entry needed).

use crate::optimize;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{
        AnnotateAble, ErrorData, GetPromptRequestParams, GetPromptResult, Implementation,
        ListPromptsResult, ListResourcesResult, PaginatedRequestParams, RawResource,
        ReadResourceRequestParams, ReadResourceResult, ResourceContents, ServerCapabilities,
        ServerInfo,
    },
    schemars,
    service::{RequestContext, RoleServer},
    tool, tool_handler, tool_router,
    transport::stdio,
    ServerHandler, ServiceExt,
};
use serde::Deserialize;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetRoutingSuggestionRequest {
    #[schemars(description = "The user's prompt to analyze")]
    prompt: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct CheckSessionHealthRequest {
    #[schemars(description = "The session ID to check")]
    session_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SkillSearchRequest {
    #[schemars(description = "What you need to do; keyword-style works best")]
    query: String,
    #[schemars(description = "Max results (default 5)")]
    #[serde(default)]
    limit: Option<usize>,
    #[schemars(description = "'all' = every word must match (default); 'any' = broader recall for retries")]
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SkillLoadRequest {
    #[schemars(description = "Skill name from skill_search; qualify as 'source:name' if ambiguous")]
    name: String,
    #[schemars(description = "true = load the original even when a compressed copy exists")]
    #[serde(default)]
    original: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GatewaySearchRequest {
    #[schemars(description = "What tool you need; keyword-style works best")]
    query: String,
    #[schemars(description = "Max results (default 5)")]
    #[serde(default)]
    limit: Option<usize>,
    #[schemars(description = "'all' = every word must match (default); 'any' = broader recall for retries")]
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GatewayExecuteRequest {
    #[schemars(description = "Server name from gateway_search")]
    server: String,
    #[schemars(description = "Tool name from gateway_search")]
    tool: String,
    // A bare `serde_json::Value` here made schemars emit a typeless schema
    // (Value can be anything), so callers had no signal to send a nested
    // JSON object rather than a stringified one — gateway_execute couldn't
    // actually be invoked with arguments. `Map` renders as `{"type":
    // ["object", "null"]}`, a real hint.
    #[schemars(description = "Arguments object matching the tool's input_schema")]
    #[serde(default)]
    args: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClaimTargetRequest {
    #[schemars(description = "Target to claim, e.g. \"issue#42\" or \"pr#7\"")]
    target: String,
    #[schemars(description = "Repo key owner/name (default: normalized origin remote)")]
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClaimListRequest {
    #[schemars(description = "Repo key owner/name (default: current repo)")]
    #[serde(default)]
    repo: Option<String>,
    #[schemars(description = "Include stale and done claims (default false)")]
    #[serde(default)]
    all: bool,
    #[schemars(description = "List across every repo in the ledger (default false)")]
    #[serde(default)]
    all_repos: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ChannelSendRequest {
    #[schemars(description = "Platform to send to: telegram, slack, or discord")]
    platform: String,
    #[schemars(description = "Recipient id: Telegram chat_id, or Slack/Discord channel id")]
    target: String,
    #[schemars(description = "The message text to send")]
    message: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReviewSubmitRequest {
    #[schemars(description = "Findings, each {file, line, message, severity?, category?}")]
    findings: Vec<serde_json::Value>,
    #[schemars(description = "Review round id (default: current branch)")]
    #[serde(default)]
    pr: Option<String>,
    #[schemars(description = "Finder name (default: detected agent)")]
    #[serde(default)]
    agent: Option<String>,
    #[schemars(description = "Repo key owner/name (default: origin remote)")]
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReviewConsensusRequest {
    #[schemars(description = "Review round id (default: current branch)")]
    #[serde(default)]
    pr: Option<String>,
    #[schemars(description = "Diff base ref (default: master)")]
    #[serde(default)]
    base: Option<String>,
    #[schemars(description = "Diff head ref (default: HEAD)")]
    #[serde(default)]
    head: Option<String>,
    #[schemars(description = "Repo key owner/name (default: origin remote)")]
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReviewRoundRequest {
    #[schemars(description = "Review round id (default: current branch)")]
    #[serde(default)]
    pr: Option<String>,
    #[schemars(description = "Repo key owner/name (default: origin remote)")]
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ArtifactPublishRequest {
    #[schemars(description = "Display name of the artifact")]
    name: String,
    #[schemars(description = "html | markdown | mermaid | diagram | text (default: text)")]
    #[serde(default)]
    r#type: Option<String>,
    #[schemars(description = "Full artifact content (HTML document, markdown source, plain text, ...)")]
    content: String,
    #[schemars(description = "Session ID for grouping artifacts (optional)")]
    #[serde(default)]
    session_id: Option<String>,
    #[schemars(description = "Existing artifact id to update in place — keeps the same URL and live-reloads open viewers")]
    #[serde(default)]
    update_id: Option<String>,
    #[schemars(description = "Short label for this version, shown in history (e.g. \"draft\", \"final\")")]
    #[serde(default)]
    label: Option<String>,
    #[schemars(description = "One-line description shown in the gallery")]
    #[serde(default)]
    description: Option<String>,
    #[schemars(description = "One or two emoji used as the page icon")]
    #[serde(default)]
    favicon: Option<String>,
    #[schemars(description = "Optimistic-concurrency guard: update only applies if the artifact's current version equals this; otherwise a version-conflict error is returned")]
    #[serde(default)]
    base_version: Option<u32>,
    #[schemars(description = "Handoff envelope: which agent/runtime is publishing (e.g. claude-code, codex)")]
    #[serde(default)]
    sender: Option<String>,
    #[schemars(description = "Handoff envelope: agent/runtime this artifact is addressed to — for WORK PRODUCTS only; facts and decisions belong in memory (engram), not artifacts")]
    #[serde(default)]
    recipient: Option<String>,
    #[schemars(description = "Handoff envelope: thread this belongs to; replies reuse the sender's thread_id")]
    #[serde(default)]
    thread_id: Option<String>,
    #[schemars(description = "Handoff envelope: artifact id this replies to")]
    #[serde(default)]
    reply_to: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ArtifactListRequest {
    #[schemars(description = "Only artifacts from this session (omit for all)")]
    #[serde(default)]
    session_id: Option<String>,
    #[schemars(description = "Inbox filter: only artifacts addressed to this agent/runtime")]
    #[serde(default)]
    recipient: Option<String>,
    #[schemars(description = "Only artifacts in this handoff thread")]
    #[serde(default)]
    thread_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ArtifactDiffRequest {
    #[schemars(description = "Artifact id")]
    id: String,
    #[schemars(description = "Older version number to diff from")]
    from_version: u32,
    #[schemars(description = "Newer version number (omit for latest)")]
    #[serde(default)]
    to_version: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ArtifactSearchRequest {
    #[schemars(description = "Case-insensitive text to find in names, descriptions, or content")]
    query: String,
    #[schemars(description = "Restrict to this session (omit for all)")]
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ArtifactGetRequest {
    #[schemars(description = "Artifact id from artifact_publish or artifact_list")]
    id: String,
    #[schemars(description = "Specific version to fetch (omit for latest)")]
    #[serde(default)]
    version: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ArtifactDeleteRequest {
    #[schemars(description = "Artifact id to delete (removes all versions)")]
    id: String,
}

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
}

/// flared's default HTTP port; its artifact routes live under /artifacts.
const FLARED_DEFAULT_PORT: u16 = 35273;
const FLARED_ARTIFACTS_PATH: &str = "/artifacts/";

/// flared's HTTP port: honor a `port` override in its config.toml when
/// readable (a `--port` CLI override is invisible here and lands on the
/// fixed-port fallback chain); default otherwise.
fn flared_port() -> u16 {
    dirs::config_dir()
        .map(|dir| dir.join("flared").join("config.toml"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| parse_flared_port(&text))
        .unwrap_or(FLARED_DEFAULT_PORT)
}

/// Extract the top-level `port` key from flared's config.toml text — a
/// minimal scan that avoids a toml dependency for one key. Absent or
/// malformed values -> None.
fn parse_flared_port(text: &str) -> Option<u16> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // TOML top-level keys end at the first table header
            return None;
        }
        if let Some(rest) = line.strip_prefix("port")
            && let Some(value) = rest.trim_start().strip_prefix('=')
        {
            return value
                .trim()
                .split(|c: char| c == '#' || c.is_whitespace())
                .next()
                .and_then(|v| v.parse().ok());
        }
    }
    None
}

/// How artifact pages reach the browser for this process.
enum ArtifactBackend {
    /// This process owns the listener.
    Owned(agentflare_artifacts::ArtifactServer),
    /// Another process serves the shared store: flared under /artifacts on
    /// its fixed port, or an earlier session's root-mounted server.
    External { port: u16, path: &'static str },
}

impl ArtifactBackend {
    /// Base URL artifact links hang off (no trailing slash).
    fn base_url(&self) -> String {
        match self {
            ArtifactBackend::Owned(server) => server.base_url(),
            ArtifactBackend::External { port, path } => {
                format!("http://127.0.0.1:{port}{}", path.trim_end_matches('/'))
            }
        }
    }
}

#[tool_router]
impl AgentflareMcp {
    #[tool(description = "Check if a session should be refreshed based on turn count and elapsed time.")]
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
                Some(nudge) => serde_json::json!({"session_id": session_id, "status": "stale", "nudge": nudge}),
                None => serde_json::json!({"session_id": session_id, "status": "healthy"}),
            },
            None => serde_json::json!({"session_id": session_id, "status": "unknown", "message": "Session not tracked"}),
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
    ) -> Result<T, ErrorData> {
        let mut guard = self
            .skills_registry
            .lock()
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        if guard.is_none() {
            let db_path = self
                .skills_db_override
                .clone()
                .unwrap_or_else(crate::paths::skills_db_path);
            let reg = skill_registry::Registry::open_default(&db_path)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            *guard = Some(reg);
        }
        let reg = guard.as_mut().expect("just initialized above");
        reg.ensure_fresh()
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(f(reg))
    }

    #[tool(description = "Search installed skills (all agents' skill dirs) by task description. Returns name, source, description, and estimated token cost; call skill_load to fetch one.")]
    fn skill_search(
        &self,
        Parameters(SkillSearchRequest { query, limit, mode }): Parameters<SkillSearchRequest>,
    ) -> Result<String, ErrorData> {
        if query.trim().is_empty() {
            return Err(ErrorData::invalid_params("query is required", None));
        }
        let mode = match mode.as_deref() {
            None | Some("all") => skill_registry::MatchMode::All,
            Some("any") => skill_registry::MatchMode::Any,
            Some(other) => {
                return Err(ErrorData::invalid_params(
                    format!("mode must be 'all' or 'any', got '{other}'"),
                    None,
                ))
            }
        };
        let hits = self
            .with_fresh_registry(|reg| reg.search(&query, limit.unwrap_or(5), mode))?
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(serde_json::to_string_pretty(&hits).unwrap_or_default())
    }

    #[tool(description = "Load a skill's full instructions by name. Serves the compressed copy when one exists (original=true for the source). Sibling reference files are listed, not inlined.")]
    fn skill_load(
        &self,
        Parameters(SkillLoadRequest { name, original }): Parameters<SkillLoadRequest>,
    ) -> Result<String, ErrorData> {
        if name.trim().is_empty() {
            return Err(ErrorData::invalid_params("name is required", None));
        }
        let result = self.with_fresh_registry(|reg| reg.load(&name, original))?;
        match result {
            Ok(s) => Ok(serde_json::to_string_pretty(&s).unwrap_or_default()),
            Err(e @ skill_registry::LoadError::NotFound(_))
            | Err(e @ skill_registry::LoadError::Ambiguous(_)) => {
                Err(ErrorData::invalid_params(e.to_string(), None))
            }
            Err(e) => Err(ErrorData::internal_error(e.to_string(), None)),
        }
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
                    let store =
                        std::sync::Arc::new(agentflare_artifacts::ArtifactStore::new(dir));
                    let server = agentflare_artifacts::ArtifactServer::start(store.clone(), 0)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    (store, ArtifactBackend::Owned(server))
                }
                None => {
                    let dir = crate::paths::home().join(".agentflare").join("artifacts");
                    let store =
                        std::sync::Arc::new(agentflare_artifacts::ArtifactStore::new(dir));
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

    #[tool(description = "Publish a live-shareable artifact page (HTML, markdown, mermaid, text, ...) and return its local URL. Pass update_id to update in place — same URL, open viewers live-reload; every publish snapshots a version. Pass base_version to fail on concurrent edits instead of clobbering.")]
    fn artifact_publish(
        &self,
        Parameters(ArtifactPublishRequest {
            name,
            r#type,
            content,
            session_id,
            update_id,
            label,
            description,
            favicon,
            base_version,
            sender,
            recipient,
            thread_id,
            reply_to,
        }): Parameters<ArtifactPublishRequest>,
    ) -> Result<String, ErrorData> {
        if name.trim().is_empty() {
            return Err(ErrorData::invalid_params("name is required", None));
        }
        if content.is_empty() {
            return Err(ErrorData::invalid_params("content is required", None));
        }
        let (store, base) = self.ensure_artifact_server()?;
        let req = agentflare_artifacts::PublishRequest {
            name,
            artifact_type: agentflare_artifacts::ArtifactType::from(
                r#type.as_deref().unwrap_or("text"),
            ),
            content,
            session_id: session_id.unwrap_or_default(),
            update_id,
            label,
            description,
            favicon,
            base_version,
            sender: sender.or_else(|| self.agent.clone()),
            recipient,
            thread_id,
            reply_to,
            git: Self::git_provenance(),
        };
        let resp = store.publish(&req).map_err(Self::artifact_error)?;
        let result = serde_json::json!({
            "id": resp.id,
            "version": resp.version,
            "url": format!("{base}/{}", resp.id),
            "index": format!("{base}/"),
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }

    /// Best-effort git context of this process's cwd (the project the MCP
    /// server was launched in). None outside a repo; never fails a publish.
    pub(crate) fn git_provenance() -> Option<agentflare_artifacts::GitProvenance> {
        fn git(args: &[&str]) -> Option<String> {
            let out = std::process::Command::new("git").args(args).output().ok()?;
            if !out.status.success() {
                return None;
            }
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!s.is_empty()).then_some(s)
        }
        let commit = git(&["rev-parse", "HEAD"])?;
        Some(agentflare_artifacts::GitProvenance {
            repo: git(&["remote", "get-url", "origin"]),
            r#ref: git(&["rev-parse", "--abbrev-ref", "HEAD"]),
            commit: Some(commit),
        })
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

    #[tool(description = "List published artifacts (id, name, type, version, description, session, handoff envelope) with their local URLs. Filter by session_id, recipient (inbox), or thread_id.")]
    fn artifact_list(
        &self,
        Parameters(ArtifactListRequest { session_id, recipient, thread_id }): Parameters<ArtifactListRequest>,
    ) -> Result<String, ErrorData> {
        let (store, base) = self.ensure_artifact_server()?;
        let summaries = store
            .list(session_id.as_deref())
            .map_err(Self::artifact_error)?;
        let items: Vec<serde_json::Value> = summaries
            .iter()
            .filter(|s| {
                recipient.as_deref().is_none_or(|r| s.recipient.as_deref() == Some(r))
                    && thread_id.as_deref().is_none_or(|t| s.thread_id.as_deref() == Some(t))
            })
            .map(|s| {
                let mut v = serde_json::to_value(s).unwrap_or_default();
                if let Some(obj) = v.as_object_mut() {
                    obj.insert(
                        "url".into(),
                        serde_json::json!(format!("{base}/{}", s.id)),
                    );
                }
                v
            })
            .collect();
        Ok(serde_json::to_string_pretty(&items).unwrap_or_default())
    }

    #[tool(description = "Fetch an artifact's full content and metadata by id; pass version to read an older snapshot. Version history itself is at GET /{id}/versions.")]
    fn artifact_get(
        &self,
        Parameters(ArtifactGetRequest { id, version }): Parameters<ArtifactGetRequest>,
    ) -> Result<String, ErrorData> {
        if id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let (store, _base) = self.ensure_artifact_server()?;
        let artifact = match version {
            Some(n) => store.get_version(&id, n),
            None => store.get(&id),
        }
        .map_err(Self::artifact_error)?;
        Ok(serde_json::to_string_pretty(&artifact).unwrap_or_default())
    }

    #[tool(description = "Unified diff between two versions of an artifact; to_version defaults to the latest. Use after an update to see what changed.")]
    fn artifact_diff(
        &self,
        Parameters(ArtifactDiffRequest { id, from_version, to_version }): Parameters<ArtifactDiffRequest>,
    ) -> Result<String, ErrorData> {
        if id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let (store, _base) = self.ensure_artifact_server()?;
        let to = match to_version {
            Some(v) => v,
            None => store.get(&id).map_err(Self::artifact_error)?.version,
        };
        store.diff(&id, from_version, to).map_err(Self::artifact_error)
    }

    #[tool(description = "Case-insensitive search across artifact names, descriptions, and content; returns matching summaries with a snippet around the first content match.")]
    fn artifact_search(
        &self,
        Parameters(ArtifactSearchRequest { query, session_id }): Parameters<ArtifactSearchRequest>,
    ) -> Result<String, ErrorData> {
        if query.trim().is_empty() {
            return Err(ErrorData::invalid_params("query is required", None));
        }
        let (store, base) = self.ensure_artifact_server()?;
        let needle = query.to_lowercase();
        let mut hits = Vec::new();
        for summary in store.list(session_id.as_deref()).map_err(Self::artifact_error)? {
            let name_hit = summary.name.to_lowercase().contains(&needle);
            let desc_hit = summary
                .description
                .as_deref()
                .is_some_and(|d| d.to_lowercase().contains(&needle));
            let content = store.get(&summary.id).map(|a| a.content).unwrap_or_default();
            let content_pos = content.to_lowercase().find(&needle);
            if !(name_hit || desc_hit || content_pos.is_some()) {
                continue;
            }
            let snippet = content_pos.map(|pos| {
                let mut start = pos.saturating_sub(40);
                while !content.is_char_boundary(start) {
                    start -= 1;
                }
                let mut end = (pos + needle.len() + 40).min(content.len());
                while !content.is_char_boundary(end) {
                    end += 1;
                }
                content[start..end].to_string()
            });
            let mut v = serde_json::to_value(&summary).unwrap_or_default();
            if let Some(obj) = v.as_object_mut() {
                obj.insert(
                    "url".into(),
                    serde_json::json!(format!("{base}/{}", summary.id)),
                );
                if let Some(snippet) = snippet {
                    obj.insert("snippet".into(), serde_json::json!(snippet));
                }
            }
            hits.push(v);
        }
        Ok(serde_json::to_string_pretty(&hits).unwrap_or_default())
    }

    #[tool(description = "Delete an artifact and all its versions by id.")]
    fn artifact_delete(
        &self,
        Parameters(ArtifactDeleteRequest { id }): Parameters<ArtifactDeleteRequest>,
    ) -> Result<String, ErrorData> {
        if id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let (store, _base) = self.ensure_artifact_server()?;
        let deleted = store.delete(&id).map_err(Self::artifact_error)?;
        Ok(serde_json::json!({ "deleted": deleted }).to_string())
    }

    #[tool(description = "Send a text message out to a chat platform (telegram, slack, or discord). The bot token must already be stored as the gateway secret '<platform>_bot_token'. target is the Telegram chat_id or Slack/Discord channel id.")]
    fn channel_send(
        &self,
        Parameters(ChannelSendRequest { platform, target, message }): Parameters<ChannelSendRequest>,
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

    #[tool(description = "Claim a GitHub issue/PR so other agents don't duplicate the work. Returns 'acquired' if you now own it, or 'held' with the current owner if a live claim exists. Only stale (past-TTL) or done claims are stolen. Re-heartbeat periodically to keep it.")]
    fn claim_acquire(
        &self,
        Parameters(ClaimTargetRequest { target, repo }): Parameters<ClaimTargetRequest>,
    ) -> Result<String, ErrorData> {
        // Only capture the current checkout's commit when the repo is
        // auto-resolved from it; an explicit repo may name a different one.
        let repo_overridden = repo.as_ref().is_some_and(|r| !r.is_empty());
        let (conn, repo) = Self::claim_ctx(&target, repo)?;
        let owner = crate::claims::owner_id();
        let commit = if repo_overridden { None } else { Self::git_provenance().and_then(|g| g.commit) };
        let outcome = crate::claims::acquire(
            &conn, &repo, &target, &owner, commit.as_deref(),
            crate::claims::now(), crate::claims::ttl_secs(),
        )
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(match outcome {
            crate::claims::Acquire::Acquired => {
                serde_json::json!({ "status": "acquired", "repo": repo, "target": target, "owner": owner })
            }
            crate::claims::Acquire::Held { owner: holder, age_secs } => {
                serde_json::json!({ "status": "held", "repo": repo, "target": target, "owner": holder, "age_secs": age_secs })
            }
        }
        .to_string())
    }

    #[tool(description = "Refresh the lease on a claim you own, so it isn't reclaimed as stale. Returns refreshed=false if the claim is gone or owned by someone else.")]
    fn claim_heartbeat(
        &self,
        Parameters(ClaimTargetRequest { target, repo }): Parameters<ClaimTargetRequest>,
    ) -> Result<String, ErrorData> {
        let (conn, repo) = Self::claim_ctx(&target, repo)?;
        let owner = crate::claims::owner_id();
        let ok = crate::claims::heartbeat(&conn, &repo, &target, &owner, crate::claims::now())
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(serde_json::json!({ "refreshed": ok, "repo": repo, "target": target }).to_string())
    }

    #[tool(description = "Release a claim you own, freeing the target for other agents. Returns released=false if it wasn't yours.")]
    fn claim_release(
        &self,
        Parameters(ClaimTargetRequest { target, repo }): Parameters<ClaimTargetRequest>,
    ) -> Result<String, ErrorData> {
        let (conn, repo) = Self::claim_ctx(&target, repo)?;
        let owner = crate::claims::owner_id();
        let ok = crate::claims::release(&conn, &repo, &target, &owner)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(serde_json::json!({ "released": ok, "repo": repo, "target": target }).to_string())
    }

    #[tool(description = "Mark a claim you own as done — keeps the audit row (unlike release, which deletes it) while freeing the target for re-acquisition. Returns done=false if it wasn't yours.")]
    fn claim_done(
        &self,
        Parameters(ClaimTargetRequest { target, repo }): Parameters<ClaimTargetRequest>,
    ) -> Result<String, ErrorData> {
        let (conn, repo) = Self::claim_ctx(&target, repo)?;
        let owner = crate::claims::owner_id();
        let ok = crate::claims::done(&conn, &repo, &target, &owner, crate::claims::now())
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(serde_json::json!({ "done": ok, "repo": repo, "target": target }).to_string())
    }

    #[tool(description = "List work claims. Defaults to live claims for the current repo; set all=true to include stale/done, all_repos=true to span every repo.")]
    fn claim_list(
        &self,
        Parameters(ClaimListRequest { repo, all, all_repos }): Parameters<ClaimListRequest>,
    ) -> Result<String, ErrorData> {
        let conn = Self::claim_db()?;
        let scope = if all_repos {
            None
        } else {
            Some(crate::claims::resolve_repo(repo).ok_or_else(|| {
                ErrorData::invalid_params(
                    "could not determine repo — run in a git repo or pass repo=owner/name (or all_repos=true)",
                    None,
                )
            })?)
        };
        let claims = crate::claims::list(&conn, scope.as_deref(), all, crate::claims::now(), crate::claims::ttl_secs())
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(serde_json::to_string_pretty(&claims).unwrap_or_default())
    }

    /// Opens the ledger db, mapping errors to MCP internal_error.
    fn claim_db() -> Result<rusqlite::Connection, ErrorData> {
        crate::db::open().map_err(|e| ErrorData::internal_error(format!("cannot open ledger: {e}"), None))
    }

    /// Shared prelude for the per-target claim tools: validate target, open the
    /// ledger, resolve the repo key.
    fn claim_ctx(target: &str, repo: Option<String>) -> Result<(rusqlite::Connection, String), ErrorData> {
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

    #[tool(description = "Submit a finder's review findings for a round (each finding is {file, line, message, severity?, category?}). Replaces this finder's prior findings for the round. Call from each reviewing agent, then call review_consensus to verify + dedup + tag.")]
    fn review_submit(
        &self,
        Parameters(ReviewSubmitRequest { findings, pr, agent, repo }): Parameters<ReviewSubmitRequest>,
    ) -> Result<String, ErrorData> {
        let conn = Self::claim_db()?;
        let repo = Self::resolve_repo_or_err(repo)?;
        let pr = Self::resolve_round(pr)?;
        let agent = agent.filter(|s| !s.is_empty()).unwrap_or_else(crate::review::submitter_name);
        let parsed: Vec<crate::review::Finding> = findings
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<_, _>>()
            .map_err(|e| ErrorData::invalid_params(format!("invalid finding: {e}"), None))?;
        let n = crate::review::submit(&conn, &repo, &pr, &agent, &parsed, crate::claims::now())
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(serde_json::json!({ "submitted": n, "repo": repo, "pr": pr, "agent": agent }).to_string())
    }

    #[tool(description = "Verify all submitted findings for a round against the git diff (base...head), dedup overlapping ones, and tag each CONFIRMED/UNIQUE/DISPUTED/UNVERIFIED. Returns the ranked consensus items.")]
    fn review_consensus(
        &self,
        Parameters(ReviewConsensusRequest { pr, base, head, repo }): Parameters<ReviewConsensusRequest>,
    ) -> Result<String, ErrorData> {
        let conn = Self::claim_db()?;
        let repo = Self::resolve_repo_or_err(repo)?;
        let pr = Self::resolve_round(pr)?;
        let findings = crate::review::load(&conn, &repo, &pr)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let diff = crate::review::compute_diff(base.as_deref(), head.as_deref())
            .map_err(|e| ErrorData::invalid_params(e, None))?;
        let changed = crate::review::changed_lines(&diff);
        let items = crate::review::consensus(&findings, &changed);
        Ok(serde_json::json!({
            "repo": repo,
            "pr": pr,
            "items": items,
            "markdown": crate::review::render_markdown(&items),
        })
        .to_string())
    }

    #[tool(description = "List the raw submitted findings for a review round (before consensus).")]
    fn review_list(
        &self,
        Parameters(ReviewRoundRequest { pr, repo }): Parameters<ReviewRoundRequest>,
    ) -> Result<String, ErrorData> {
        let conn = Self::claim_db()?;
        let repo = Self::resolve_repo_or_err(repo)?;
        let pr = Self::resolve_round(pr)?;
        let findings = crate::review::load(&conn, &repo, &pr)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let rows: Vec<serde_json::Value> = findings
            .iter()
            .map(|sf| serde_json::json!({ "agent": sf.agent, "file": sf.finding.file, "line": sf.finding.line, "message": sf.finding.message, "severity": sf.finding.severity }))
            .collect();
        Ok(serde_json::to_string_pretty(&rows).unwrap_or_default())
    }

    #[tool(description = "Drop all submitted findings for a review round.")]
    fn review_clear(
        &self,
        Parameters(ReviewRoundRequest { pr, repo }): Parameters<ReviewRoundRequest>,
    ) -> Result<String, ErrorData> {
        let conn = Self::claim_db()?;
        let repo = Self::resolve_repo_or_err(repo)?;
        let pr = Self::resolve_round(pr)?;
        let n = crate::review::clear(&conn, &repo, &pr)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(serde_json::json!({ "cleared": n, "repo": repo, "pr": pr }).to_string())
    }

    fn resolve_repo_or_err(repo: Option<String>) -> Result<String, ErrorData> {
        crate::claims::resolve_repo(repo).ok_or_else(|| {
            ErrorData::invalid_params("could not determine repo — run in a git repo or pass repo=owner/name", None)
        })
    }

    /// Review round id: explicit `pr`, else the current branch name.
    fn resolve_round(pr: Option<String>) -> Result<String, ErrorData> {
        if let Some(pr) = pr.filter(|s| !s.is_empty()) {
            return Ok(pr);
        }
        std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ErrorData::invalid_params("could not determine round — pass pr", None))
    }

    fn gateway_db_path() -> std::path::PathBuf {
        dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("agentflare").join("gateway.db")
    }

    fn load_gateway_config() -> gateway_registry::GatewayConfig {
        let path = crate::paths::home().join(".agentflare").join("gateway.toml");
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
            .filter_map(|name| match crate::gateway_secrets::get_secret(&conn, &name) {
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
            })
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
    /// with `gateway_search`; that doesn't compile without unstable
    /// async-closure/HRTB machinery, so each tool method just calls this
    /// helper and then works with the guard itself.)
    async fn ensure_gateway_registry(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, Option<gateway_registry::Registry>>, ErrorData> {
        let mut guard = self.gateway_registry.lock().await;
        if guard.is_none() {
            let db_path = self.gateway_db_override.clone().unwrap_or_else(Self::gateway_db_path);
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

    #[tool(description = "Search downstream MCP servers' tools by task description. Returns server, tool, description, and input_schema; call gateway_execute to run one.")]
    async fn gateway_search(
        &self,
        Parameters(GatewaySearchRequest { query, limit, mode }): Parameters<GatewaySearchRequest>,
    ) -> Result<String, ErrorData> {
        if query.trim().is_empty() {
            return Err(ErrorData::invalid_params("query is required", None));
        }
        let mode = match mode.as_deref() {
            None | Some("all") => gateway_registry::MatchMode::All,
            Some("any") => gateway_registry::MatchMode::Any,
            Some(other) => {
                return Err(ErrorData::invalid_params(format!("mode must be 'all' or 'any', got '{other}'"), None))
            }
        };
        let guard = self.ensure_gateway_registry().await?;
        let reg = guard.as_ref().expect("ensured above");
        let hits = reg
            .search(&query, limit.unwrap_or(5), mode)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        Ok(serde_json::to_string_pretty(&hits).unwrap_or_default())
    }

    #[tool(description = "Execute a tool on a downstream MCP server found via gateway_search. args must match that tool's input_schema.")]
    async fn gateway_execute(
        &self,
        Parameters(GatewayExecuteRequest { server, tool, args }): Parameters<GatewayExecuteRequest>,
    ) -> Result<String, ErrorData> {
        if server.trim().is_empty() || tool.trim().is_empty() {
            return Err(ErrorData::invalid_params("server and tool are required", None));
        }
        let args = args.map(serde_json::Value::Object).unwrap_or(serde_json::Value::Null);
        let guard = self.ensure_gateway_registry().await?;
        let reg = guard.as_ref().expect("ensured above");
        match reg.execute(&server, &tool, args).await {
            Ok(value) => {
                let capped = gateway_registry::truncate_if_needed(&value, gateway_registry::DEFAULT_MAX_CHARS);
                Ok(serde_json::to_string_pretty(&capped).unwrap_or_default())
            }
            Err(e @ gateway_registry::GatewayError::ServerNotFound(_))
            | Err(e @ gateway_registry::GatewayError::ToolNotFound(_))
            | Err(e @ gateway_registry::GatewayError::InvalidArgument(_)) => {
                Err(ErrorData::invalid_params(e.to_string(), None))
            }
            // Every other variant (Upstream, Connection, Timeout, ...)
            // carries the downstream server's or OS's own error text
            // verbatim — unlike the three above, which are our own
            // controlled messages. Redact before it reaches the LLM: a
            // downstream server's raw error could otherwise leak a file
            // path, connection string, or an echoed credential.
            Err(e) => Err(ErrorData::internal_error(gateway_registry::redact_error_for_llm(&e.to_string()), None)),
        }
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
            })).unwrap_or_default(),
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

#[tool_handler]
impl ServerHandler for AgentflareMcp {
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
        explicit.filter(|s| !s.is_empty()).or_else(agent_detector::agent_name)
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
mod tests {
    use super::*;

    #[test]
    fn parse_flared_port_reads_top_level_key_only() {
        assert_eq!(parse_flared_port("port = 4444\n"), Some(4444));
        assert_eq!(parse_flared_port("# comment\nport=9999 # inline\n"), Some(9999));
        // tables end the top-level scan; a port inside one is not flared's
        assert_eq!(parse_flared_port("[[registries]]\nport = 1\n"), None);
        // prefix collisions and malformed values are not overrides
        assert_eq!(parse_flared_port("portable = 1\nlight_interval_secs = 60\n"), None);
        assert_eq!(parse_flared_port("port = not-a-number\n"), None);
        assert_eq!(parse_flared_port(""), None);
    }

    #[test]
    fn get_info_reports_agentflare_identity() {
        let s = AgentflareMcp::default();
        let info = s.get_info();
        assert_eq!(info.server_info.name, env!("CARGO_PKG_NAME"));
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn routing_suggestion_returns_null_for_non_locate() {
        let s = AgentflareMcp::default();
        let result = s.get_routing_suggestion(Parameters(GetRoutingSuggestionRequest {
            prompt: "refactor the payment module".to_string(),
        }));
        assert!(result.contains("null"));
    }

    #[test]
    fn routing_suggestion_returns_nudge_for_find() {
        let s = AgentflareMcp::default();
        let result = s.get_routing_suggestion(Parameters(GetRoutingSuggestionRequest {
            prompt: "find the auth handler".to_string(),
        }));
        assert!(result.contains("cheap-model"));
    }

    #[test]
    fn check_session_health_unknown_returns_status() {
        let s = AgentflareMcp::default();
        let result = s
            .check_session_health(Parameters(CheckSessionHealthRequest {
                session_id: "nonexistent-session-id".to_string(),
            }))
            .unwrap();
        assert!(result.contains("unknown"));
    }

    #[test]
    fn check_session_health_rejects_empty_session_id() {
        let s = AgentflareMcp::default();
        let err = s
            .check_session_health(Parameters(CheckSessionHealthRequest {
                session_id: String::new(),
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    // NOTE: `list_resources`/`read_resource` on `ServerHandler` take a
    // `RequestContext<RoleServer>`, which embeds a `Peer<RoleServer>` whose
    // constructor is `pub(crate)` inside rmcp (and requires the `client`
    // feature this crate doesn't enable) — there is no supported way to
    // build one from outside the rmcp crate. The URI-dispatch logic is
    // therefore extracted into `list_resources_sync`/`read_resource_sync`
    // (plain sync methods with identical bodies to the trait methods) so it
    // can be unit-tested directly; the trait methods are thin async shells
    // over them.
    //
    // `agentflare://sessions` is deliberately NOT covered here: it reads
    // mutable on-disk runtime state via `optimize::load_runtime()`, whose
    // path (`crate::state::state_dir()/runtime-state.json`) is not
    // injectable, so exercising it deterministically would mean reading (or
    // mutating) the real shared user state file.

    #[test]
    fn list_resources_returns_sessions_and_nudges() {
        let s = AgentflareMcp::default();
        let result = s.list_resources_sync();
        let uris: Vec<&str> = result.resources.iter().map(|r| r.uri.as_str()).collect();
        assert_eq!(uris, vec!["agentflare://sessions", "agentflare://nudges"]);
    }

    #[test]
    fn read_resource_nudges_returns_nudges_json() {
        let s = AgentflareMcp::default();
        let result = s.read_resource_sync("agentflare://nudges").unwrap();
        assert_eq!(result.contents.len(), 1);
        let ResourceContents::TextResourceContents { text, uri, .. } = &result.contents[0] else {
            panic!("expected text resource contents");
        };
        assert_eq!(uri, "agentflare://nudges");
        assert!(text.contains("session_hygiene"));
    }

    #[test]
    fn read_resource_unknown_uri_returns_resource_not_found() {
        let s = AgentflareMcp::default();
        let err = s.read_resource_sync("agentflare://bogus").unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
    }

    #[test]
    fn skill_search_empty_query_is_invalid_params() {
        let s = AgentflareMcp::default();
        let err = s
            .skill_search(Parameters(SkillSearchRequest {
                query: "".into(),
                limit: None,
                mode: None,
            }))
            .unwrap_err();
        assert!(err.to_string().contains("query"));
    }

    #[test]
    fn skill_load_unknown_name_reports_not_found_with_search_hint() {
        // Isolated DB path so the test never opens/refreshes the shared skills.db.
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            skills_db_override: Some(tmp.path().join("skills.db")),
            ..Default::default()
        };
        let out = s
            .skill_load(Parameters(SkillLoadRequest {
                name: "definitely-not-a-skill-xyz".into(),
                original: false,
            }))
            .unwrap_err();
        assert!(out.to_string().contains("skill_search"));
    }

    #[test]
    fn skill_search_mode_rejects_unknown_value() {
        let s = AgentflareMcp::default();
        let err = s
            .skill_search(Parameters(SkillSearchRequest {
                query: "anything".into(),
                limit: None,
                mode: Some("fuzzy".into()),
            }))
            .unwrap_err();
        assert!(err.to_string().contains("mode"));
    }

    #[tokio::test]
    async fn gateway_search_empty_query_is_invalid_params() {
        // Isolated DB path so the test never opens/refreshes the shared gateway.db.
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            gateway_db_override: Some(tmp.path().join("gateway.db")),
            ..Default::default()
        };
        let err = s
            .gateway_search(Parameters(GatewaySearchRequest { query: "".into(), limit: None, mode: None }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("query is required"));
    }

    #[tokio::test]
    async fn gateway_search_mode_rejects_unknown_value() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            gateway_db_override: Some(tmp.path().join("gateway.db")),
            ..Default::default()
        };
        let err = s
            .gateway_search(Parameters(GatewaySearchRequest {
                query: "x".into(),
                limit: None,
                mode: Some("bogus".into()),
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("mode must be"));
    }

    #[tokio::test]
    async fn gateway_execute_requires_server_and_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            gateway_db_override: Some(tmp.path().join("gateway.db")),
            ..Default::default()
        };
        let err = s
            .gateway_execute(Parameters(GatewayExecuteRequest {
                server: "".into(),
                tool: "x".into(),
                args: Some(serde_json::Map::new()),
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("required"));
    }

    #[tokio::test]
    async fn gateway_execute_unknown_server_is_invalid_params() {
        // Isolated DB path, no servers configured — `Registry::execute` is
        // guaranteed to hit `GatewayError::ServerNotFound`, which must map to
        // `invalid_params` (a caller-fixable mistake), not `internal_error`.
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            gateway_db_override: Some(tmp.path().join("gateway.db")),
            ..Default::default()
        };
        let err = s
            .gateway_execute(Parameters(GatewayExecuteRequest {
                server: "definitely-not-a-configured-server".into(),
                tool: "x".into(),
                args: Some(serde_json::Map::new()),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn gateway_execute_args_schema_is_object_or_null() {
        let schema = schemars::schema_for!(GatewayExecuteRequest);
        let schema_json = serde_json::to_value(&schema).unwrap();
        let args_schema = schema_json.get("properties").and_then(|p| p.get("args")).expect("args schema present");
        let rendered = args_schema.to_string();
        assert!(rendered.contains("\"object\""), "{rendered}");
        assert!(rendered.contains("\"null\""), "{rendered}");
    }

    /// Minimal HTTP GET against a `http://127.0.0.1:<port>/<id>` URL,
    /// returning the full response (status line + headers + body).
    fn http_get(url: &str) -> String {
        use std::io::{Read, Write};
        let rest = url.strip_prefix("http://").expect("http url");
        let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
        let mut stream = std::net::TcpStream::connect(host_port)
            .unwrap_or_else(|_| panic!("connect to {host_port}"));
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        write!(stream, "GET /{path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
        stream.flush().unwrap();
        let mut full = String::new();
        let _ = stream.read_to_string(&mut full);
        full
    }

    #[test]
    fn artifact_publish_serves_content_at_returned_url() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let out = s
            .artifact_publish(Parameters(ArtifactPublishRequest {
                name: "hello".into(),
                r#type: None,
                content: "artifact-body-marker".into(),
                session_id: None,
                update_id: None,
                ..Default::default()
            }))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let url = v["url"].as_str().expect("url in response");
        assert!(url.starts_with("http://127.0.0.1:"), "local url: {url}");
        assert!(!v["id"].as_str().unwrap_or_default().is_empty());

        let resp = http_get(url);
        assert!(resp.contains("200"), "serves published artifact: {resp}");
        assert!(resp.contains("artifact-body-marker"), "body present: {resp}");
    }

    #[test]
    fn artifact_publish_update_id_keeps_same_id() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let first: serde_json::Value = serde_json::from_str(
            &s.artifact_publish(Parameters(ArtifactPublishRequest {
                name: "doc".into(),
                r#type: Some("markdown".into()),
                content: "v1".into(),
                session_id: Some("ses-1".into()),
                update_id: None,
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let id = first["id"].as_str().unwrap().to_string();

        let second: serde_json::Value = serde_json::from_str(
            &s.artifact_publish(Parameters(ArtifactPublishRequest {
                name: "doc".into(),
                r#type: Some("markdown".into()),
                content: "v2".into(),
                session_id: Some("ses-1".into()),
                update_id: Some(id.clone()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(second["id"].as_str().unwrap(), id);
        assert_eq!(second["url"], first["url"]);
    }

    #[test]
    fn artifact_list_get_delete_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let publish = |name: &str, session: &str| -> serde_json::Value {
            serde_json::from_str(
                &s.artifact_publish(Parameters(ArtifactPublishRequest {
                    name: name.into(),
                    r#type: None,
                    content: format!("content-of-{name}"),
                    session_id: Some(session.into()),
                    update_id: None,
                    description: Some(format!("desc-{name}")),
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap()
        };
        let a = publish("alpha", "ses-1");
        let _b = publish("beta", "ses-2");

        let all: serde_json::Value =
            serde_json::from_str(&s.artifact_list(Parameters(ArtifactListRequest::default())).unwrap())
                .unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);

        let one: serde_json::Value = serde_json::from_str(
            &s.artifact_list(Parameters(ArtifactListRequest {
                session_id: Some("ses-1".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(one.as_array().unwrap().len(), 1);
        assert_eq!(one[0]["name"], "alpha");
        assert_eq!(one[0]["description"], "desc-alpha");

        let id = a["id"].as_str().unwrap().to_string();
        let got: serde_json::Value = serde_json::from_str(
            &s.artifact_get(Parameters(ArtifactGetRequest { id: id.clone(), version: None })).unwrap(),
        )
        .unwrap();
        assert_eq!(got["content"], "content-of-alpha");

        let del: serde_json::Value = serde_json::from_str(
            &s.artifact_delete(Parameters(ArtifactDeleteRequest { id: id.clone() })).unwrap(),
        )
        .unwrap();
        assert_eq!(del["deleted"], true);

        let err = s
            .artifact_get(Parameters(ArtifactGetRequest { id, version: None }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn artifact_publish_version_and_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let first: serde_json::Value = serde_json::from_str(
            &s.artifact_publish(Parameters(ArtifactPublishRequest {
                name: "doc".into(),
                r#type: None,
                content: "v1".into(),
                session_id: None,
                update_id: None,
                label: Some("draft".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(first["version"], 1);
        let id = first["id"].as_str().unwrap().to_string();

        // stale base_version maps to invalid_params, not internal_error
        let update = |base: Option<u32>, content: &str| {
            s.artifact_publish(Parameters(ArtifactPublishRequest {
                name: "doc".into(),
                r#type: None,
                content: content.into(),
                session_id: None,
                update_id: Some(id.clone()),
                base_version: base,
                ..Default::default()
            }))
        };
        let second: serde_json::Value = serde_json::from_str(&update(Some(1), "v2").unwrap()).unwrap();
        assert_eq!(second["version"], 2);

        let err = update(Some(1), "v3-stale").unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(err.to_string().contains("conflict"), "{err}");
    }

    #[test]
    fn artifact_list_filters_by_recipient_and_thread() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let publish = |name: &str, recipient: Option<&str>, thread: Option<&str>| -> serde_json::Value {
            serde_json::from_str(
                &s.artifact_publish(Parameters(ArtifactPublishRequest {
                    name: name.into(),
                    content: format!("content {name}"),
                    sender: Some("claude-code".into()),
                    recipient: recipient.map(Into::into),
                    thread_id: thread.map(Into::into),
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap()
        };
        publish("packet", Some("codex"), Some("t1"));
        publish("reply", Some("claude-code"), Some("t1"));
        publish("other", None, None);

        let inbox: serde_json::Value = serde_json::from_str(
            &s.artifact_list(Parameters(ArtifactListRequest {
                recipient: Some("codex".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(inbox.as_array().unwrap().len(), 1);
        assert_eq!(inbox[0]["name"], "packet");

        let thread: serde_json::Value = serde_json::from_str(
            &s.artifact_list(Parameters(ArtifactListRequest {
                thread_id: Some("t1".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(thread.as_array().unwrap().len(), 2);
    }

    #[test]
    fn artifact_diff_tool_returns_unified_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let first: serde_json::Value = serde_json::from_str(
            &s.artifact_publish(Parameters(ArtifactPublishRequest {
                name: "doc".into(),
                content: "alpha\nbeta\n".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let id = first["id"].as_str().unwrap().to_string();
        s.artifact_publish(Parameters(ArtifactPublishRequest {
            name: "doc".into(),
            content: "alpha\ngamma\n".into(),
            update_id: Some(id.clone()),
            ..Default::default()
        }))
        .unwrap();

        // to_version omitted = latest
        let diff = s
            .artifact_diff(Parameters(ArtifactDiffRequest {
                id,
                from_version: 1,
                to_version: None,
            }))
            .unwrap();
        assert!(diff.contains("-beta"), "{diff}");
        assert!(diff.contains("+gamma"), "{diff}");
    }

    #[test]
    fn artifact_search_matches_name_description_and_content() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        s.artifact_publish(Parameters(ArtifactPublishRequest {
            name: "alpha".into(),
            content: "there is a hidden NEEDLE in here".into(),
            ..Default::default()
        }))
        .unwrap();
        s.artifact_publish(Parameters(ArtifactPublishRequest {
            name: "beta".into(),
            content: "nothing to see".into(),
            ..Default::default()
        }))
        .unwrap();

        let hits: serde_json::Value = serde_json::from_str(
            &s.artifact_search(Parameters(ArtifactSearchRequest {
                query: "needle".into(),
                session_id: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 1);
        assert_eq!(hits[0]["name"], "alpha");
        assert!(
            hits[0]["snippet"].as_str().unwrap().to_lowercase().contains("needle"),
            "{hits}"
        );

        let by_name: serde_json::Value = serde_json::from_str(
            &s.artifact_search(Parameters(ArtifactSearchRequest {
                query: "beta".into(),
                session_id: None,
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(by_name.as_array().unwrap().len(), 1);
    }

    #[test]
    fn artifact_publish_captures_git_provenance_in_repo() {
        // Tests run with cwd inside this git repo, so capture must succeed.
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let out: serde_json::Value = serde_json::from_str(
            &s.artifact_publish(Parameters(ArtifactPublishRequest {
                name: "prov".into(),
                content: "x".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let got: serde_json::Value = serde_json::from_str(
            &s.artifact_get(Parameters(ArtifactGetRequest {
                id: out["id"].as_str().unwrap().into(),
                version: None,
            }))
            .unwrap(),
        )
        .unwrap();
        let commit = got["git"]["commit"].as_str().expect("git commit captured");
        assert!(commit.len() >= 7, "{got}");
    }

    #[test]
    fn artifact_publish_defaults_sender_to_agent_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            agent: Some("opencode".into()),
            ..Default::default()
        };
        let sender_of = |req: ArtifactPublishRequest| -> serde_json::Value {
            let out: serde_json::Value =
                serde_json::from_str(&s.artifact_publish(Parameters(req)).unwrap()).unwrap();
            let got: serde_json::Value = serde_json::from_str(
                &s.artifact_get(Parameters(ArtifactGetRequest {
                    id: out["id"].as_str().unwrap().into(),
                    version: None,
                }))
                .unwrap(),
            )
            .unwrap();
            got["sender"].clone()
        };

        let defaulted = sender_of(ArtifactPublishRequest {
            name: "defaulted".into(),
            content: "x".into(),
            ..Default::default()
        });
        assert_eq!(defaulted, "opencode");

        // An explicit sender always wins over the identity default.
        let explicit = sender_of(ArtifactPublishRequest {
            name: "explicit".into(),
            content: "x".into(),
            sender: Some("codex".into()),
            ..Default::default()
        });
        assert_eq!(explicit, "codex");
    }

    #[test]
    fn identity_prefers_explicit_override_then_detection() {
        // Explicit override beats detection…
        assert_eq!(
            AgentflareMcp::identity(Some("opencode".into())).as_deref(),
            Some("opencode")
        );
        // …empty counts as unset, and without an override identity falls
        // back to detecting the host that launched us (None outside agents).
        assert_eq!(AgentflareMcp::identity(Some(String::new())), agent_detector::agent_name());
        assert_eq!(AgentflareMcp::identity(None), agent_detector::agent_name());
    }

    #[test]
    fn artifact_publish_rejects_empty_name_and_content() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        for (name, content) in [("", "x"), ("x", "")] {
            let err = s
                .artifact_publish(Parameters(ArtifactPublishRequest {
                    name: name.into(),
                    r#type: None,
                    content: content.into(),
                    session_id: None,
                    update_id: None,
                    ..Default::default()
                }))
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        }
    }
}
