//! MCP (Model Context Protocol) server over stdio, built on the `rmcp` crate
//! (`modelcontextprotocol/rust-sdk`, published to crates.io — a normal
//! dependency, not ported code; no /NOTICE entry needed).

mod item;

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

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct SkillRequest {
    #[schemars(description = "Action: search|load")]
    action: String,
    #[schemars(description = "What you need to do; keyword-style works best (search)")]
    #[serde(default)]
    query: Option<String>,
    #[schemars(description = "Skill name; qualify as 'source:name' if ambiguous (load)")]
    #[serde(default)]
    name: Option<String>,
    #[schemars(description = "Max results (default 5) (search)")]
    #[serde(default)]
    limit: Option<usize>,
    #[schemars(
        description = "'all' = every word must match (default); 'any' = broader recall for retries (search)"
    )]
    #[serde(default)]
    mode: Option<String>,
    #[schemars(description = "true = load the original even when a compressed copy exists (load)")]
    #[serde(default)]
    original: bool,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ToolRequest {
    #[schemars(description = "Action: search|execute")]
    action: String,
    #[schemars(description = "What tool you need; keyword-style works best (search)")]
    #[serde(default)]
    query: Option<String>,
    #[schemars(description = "Max results (default 5) (search)")]
    #[serde(default)]
    limit: Option<usize>,
    #[schemars(
        description = "'all' = every word must match (default); 'any' = broader recall for retries (search)"
    )]
    #[serde(default)]
    mode: Option<String>,
    #[schemars(description = "Server name from the search action (execute)")]
    #[serde(default)]
    server: Option<String>,
    #[schemars(description = "Tool name from the search action (execute)")]
    #[serde(default)]
    tool: Option<String>,
    // A bare `serde_json::Value` here made schemars emit a typeless schema
    // (Value can be anything), so callers had no signal to send a nested
    // JSON object rather than a stringified one — execute couldn't actually
    // be invoked with arguments. `Map` renders as `{"type": ["object",
    // "null"]}`, a real hint.
    #[schemars(description = "Arguments object matching the tool's input_schema (execute)")]
    #[serde(default)]
    args: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ClaimRequest {
    #[schemars(description = "Action: acquire|done|heartbeat|list|release")]
    action: String,
    #[schemars(description = "Target to claim, e.g. \"issue#42\" or \"pr#7\"")]
    #[serde(default)]
    target: Option<String>,
    #[schemars(description = "Repo key owner/name (default: normalized origin remote)")]
    #[serde(default)]
    repo: Option<String>,
    #[schemars(description = "Include stale and done claims (default false) (list)")]
    #[serde(default)]
    all: bool,
    #[schemars(description = "List across every repo in the ledger (default false) (list)")]
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

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ReviewRequest {
    #[schemars(description = "Action: clear|consensus|list|record|scores|submit")]
    action: String,
    #[schemars(
        description = "Findings, each {file, line, message, severity?, category?} (submit)"
    )]
    #[serde(default)]
    findings: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Review round id (default: current branch)")]
    #[serde(default)]
    pr: Option<String>,
    #[schemars(description = "Finder name (default: detected agent) (submit)")]
    #[serde(default)]
    agent: Option<String>,
    #[schemars(description = "Diff base ref (default: master) (consensus, record)")]
    #[serde(default)]
    base: Option<String>,
    #[schemars(description = "Diff head ref (default: HEAD) (consensus, record)")]
    #[serde(default)]
    head: Option<String>,
    #[schemars(description = "Repo key owner/name (default: origin remote)")]
    #[serde(default)]
    repo: Option<String>,
    #[schemars(description = "Aggregate across every repo (default false) (scores)")]
    #[serde(default)]
    all_repos: bool,
}

/// A handoff assigns an item to another agent and attaches the work product
/// to it as an asset. Unlike a bare item update, `recipient` is a required
/// field, not `Option` — the schema itself makes an unaddressed handoff
/// unrepresentable, so an intended handoff can't silently land with no
/// assignee. Re-attaching under the same `item_id` (or the same generated
/// filename on a freshly created item) becomes the next asset version, not
/// a duplicate.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct HandoffRequest {
    #[schemars(
        description = "Agent/runtime this handoff is addressed to — becomes the item's assignee_agent. Required."
    )]
    recipient: String,
    #[schemars(
        description = "Short name/brief for the handoff — the item's name when creating one"
    )]
    name: String,
    #[schemars(
        description = "The work product being handed off (diff, review, document, ...). Prepend the brief so the recipient knows the ask. Attached to the item as an asset."
    )]
    content: String,
    #[schemars(
        description = "html | markdown | mermaid | diagram | text (default: markdown) — picks the attached asset's extension/mime type"
    )]
    #[serde(default)]
    r#type: Option<String>,
    #[schemars(
        description = "Existing item ID to assign and attach to, instead of creating a new item"
    )]
    #[serde(default)]
    item_id: Option<String>,
    #[schemars(
        description = "Handoff thread to continue; omit to start a new one. Stored in the new item's metadata, or the attached asset's metadata when item_id is given."
    )]
    #[serde(default)]
    thread_id: Option<String>,
    #[schemars(
        description = "Id this replies to (when answering an inbox item) — stored in the attached asset's metadata for provenance"
    )]
    #[serde(default)]
    reply_to: Option<String>,
    #[schemars(
        description = "One-line description; used as the new item's description when creating one"
    )]
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ArtifactRequest {
    #[schemars(description = "Action: delete|diff|get|list|publish|search")]
    action: String,
    #[schemars(description = "Artifact id")]
    #[serde(default)]
    id: Option<String>,
    #[schemars(description = "Display name of the artifact (publish)")]
    #[serde(default)]
    name: Option<String>,
    #[schemars(
        description = "html | markdown | mermaid | diagram | text (default: text) (publish)"
    )]
    #[serde(default)]
    r#type: Option<String>,
    #[schemars(
        description = "Full artifact content (HTML document, markdown source, plain text, ...) (publish)"
    )]
    #[serde(default)]
    content: Option<String>,
    #[schemars(description = "Session ID for grouping artifacts (optional)")]
    #[serde(default)]
    session_id: Option<String>,
    #[schemars(
        description = "Existing artifact id to update in place — keeps the same URL and live-reloads open viewers (publish)"
    )]
    #[serde(default)]
    update_id: Option<String>,
    #[schemars(
        description = "Short label for this version, shown in history (e.g. \"draft\", \"final\") (publish)"
    )]
    #[serde(default)]
    label: Option<String>,
    #[schemars(description = "One-line description shown in the gallery (publish)")]
    #[serde(default)]
    description: Option<String>,
    #[schemars(description = "One or two emoji used as the page icon (publish)")]
    #[serde(default)]
    favicon: Option<String>,
    #[schemars(
        description = "Optimistic-concurrency guard: update only applies if the artifact's current version equals this; otherwise a version-conflict error is returned (publish)"
    )]
    #[serde(default)]
    base_version: Option<u32>,
    #[schemars(
        description = "Handoff envelope: agent/runtime this artifact is addressed to — for WORK PRODUCTS only; facts and decisions belong in memory (memory_remember), not artifacts (publish)"
    )]
    #[serde(default)]
    recipient: Option<String>,
    #[schemars(
        description = "Handoff envelope: thread this belongs to; replies reuse the sender's thread_id (publish)"
    )]
    #[serde(default)]
    thread_id: Option<String>,
    #[schemars(description = "Handoff envelope: artifact id this replies to (publish)")]
    #[serde(default)]
    reply_to: Option<String>,
    #[schemars(description = "Older version number to diff from (diff)")]
    #[serde(default)]
    from_version: Option<u32>,
    #[schemars(description = "Newer version number (omit for latest) (diff)")]
    #[serde(default)]
    to_version: Option<u32>,
    #[schemars(
        description = "Case-insensitive text to find in names, descriptions, or content (search)"
    )]
    #[serde(default)]
    query: Option<String>,
    #[schemars(description = "Specific version to fetch (omit for latest) (get)")]
    #[serde(default)]
    version: Option<u32>,
    #[schemars(
        description = "Inbox filter: only artifacts addressed to this agent/runtime (list)"
    )]
    #[serde(default)]
    inbox_recipient: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct OptimizeRequest {
    #[schemars(description = "Action: retrieve | list")]
    action: String,
    #[serde(default)]
    #[schemars(description = "Registered compression id (required for retrieve)")]
    id: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct MemoryRequest {
    #[schemars(description = "Action: compact|context|curate|handoff|recall|relate|remember")]
    action: String,
    #[schemars(description = "Title of the observation (remember)")]
    #[serde(default)]
    title: Option<String>,
    #[schemars(description = "Content body of the observation (remember, curate)")]
    #[serde(default)]
    content: Option<String>,
    #[schemars(
        description = "Type: decision|bugfix|discovery|pattern|learning|manual (remember, recall)"
    )]
    #[serde(default)]
    r#type: Option<String>,
    #[schemars(description = "Session ID to associate with")]
    #[serde(default)]
    session_id: Option<String>,
    #[schemars(description = "Project name")]
    #[serde(default)]
    project: Option<String>,
    #[schemars(description = "Stable topic key for upsert dedup (remember)")]
    #[serde(default)]
    topic_key: Option<String>,
    #[schemars(description = "Scope: project (default) or personal (remember)")]
    #[serde(default)]
    scope: Option<String>,
    #[schemars(description = "Search query (FTS5 BM25); omit for recent listing (recall)")]
    #[serde(default)]
    query: Option<String>,
    #[schemars(description = "Direct lookup by ID (recall)")]
    #[serde(default)]
    id: Option<i64>,
    #[schemars(description = "Max results (default 10, max 50) (recall)")]
    #[serde(default)]
    limit: Option<usize>,
    #[schemars(description = "Session summary (handoff)")]
    #[serde(default)]
    summary: Option<String>,
    #[schemars(description = "Findings array [{file, line?, summary}] (handoff)")]
    #[serde(default)]
    findings: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Decisions array [{summary, rationale?}] (handoff)")]
    #[serde(default)]
    decisions: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Files touched array [{path, modified?, tokens}] (handoff)")]
    #[serde(default)]
    files_touched: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Evidence array [{kind, action, detail}] (handoff)")]
    #[serde(default)]
    evidence: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Source observation ID (relate)")]
    #[serde(default)]
    source_id: Option<i64>,
    #[schemars(description = "Target observation ID (relate)")]
    #[serde(default)]
    target_id: Option<i64>,
    #[schemars(
        description = "Relation: related|compatible|scoped|conflicts_with|supersedes|not_conflict (relate)"
    )]
    #[serde(default)]
    relation: Option<String>,
    #[schemars(description = "Reason for the relation (relate)")]
    #[serde(default)]
    reason: Option<String>,
    #[schemars(description = "Confidence score 0.0..1.0 (relate)")]
    #[serde(default)]
    confidence: Option<f64>,
    #[schemars(description = "Pin status (curate pin/unpin actions)")]
    #[serde(default)]
    pinned: Option<bool>,
    #[schemars(description = "Sub-action for curate: update|delete|pin|unpin")]
    #[serde(default)]
    curate_action: Option<String>,
    #[schemars(description = "Target fraction of lines to keep (0.0-1.0, compact)")]
    #[serde(default)]
    compression_ratio: Option<f64>,
    #[schemars(description = "Keep N most recent messages verbatim (compact)")]
    #[serde(default)]
    preserve_recent: Option<usize>,
    #[schemars(description = "Scorer backend: fts5 (compact)")]
    #[serde(default)]
    scorer: Option<String>,
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

/// All local artifact backends (flared, another session, or our own
/// owned server) bind loopback-only — never advertise anything else.
const LOCAL_HOST: &str = "127.0.0.1";
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
                format!("http://{LOCAL_HOST}:{port}{}", path.trim_end_matches('/'))
            }
        }
    }
}

// --- agentflare-backend MCP tools -----------------------------------------
//
// Workspace is fully hidden: exactly one per system, auto-created lazily on
// first use. Project is Vercel-style auto-linked:
// `.agentflare/project.json` at the repo root maps this checkout to
// a project, created on first use and re-linked (never duplicated — see
// `resolve_project`) if the link file goes missing. Neither workspace_id nor
// project_id is ever an MCP-exposed parameter; every tool resolves
// both from cwd/git context.

/// The `.agentflare/project.json` link file's shape.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ProjectLink {
    workspace_id: String,
    project_id: String,
    identifier: String,
}

/// Default 4h — item claims are plausibly longer-running than
/// `src/claims.rs`'s 30-min GitHub-issue-claim default, hence a separate env
/// var rather than sharing `AGENTFLARE_CLAIM_TTL_SECS`.
fn backend_claim_ttl_secs() -> i64 {
    std::env::var("AGENTFLARE_BACKEND_CLAIM_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(14400) as i64
}

/// NotFound/Duplicate/InvalidTransition are caller-fixable → invalid_params;
/// a raw database error is ours to fix → internal_error. Same split as
/// `skill_load`'s NotFound/Ambiguous handling above.
fn map_backend_err(e: agentflare_backend::Error) -> ErrorData {
    match e {
        agentflare_backend::Error::NotFound(msg)
        | agentflare_backend::Error::Duplicate(msg)
        | agentflare_backend::Error::InvalidTransition(msg)
        | agentflare_backend::Error::Validation(msg) => ErrorData::invalid_params(msg, None),
        agentflare_backend::Error::Database(e) => ErrorData::internal_error(e.to_string(), None),
    }
}

/// Converts the unified dispatch-layer error type once, at whichever `?`
/// first needs an `ErrorData` — lets internal helpers (`with_fresh_registry`,
/// `claim_db`, `resolve_workspace_id`, ...) chain heterogeneous fallible
/// steps with `?` instead of mapping each one to `ErrorData` individually.
impl From<crate::errors::AgentflareError> for ErrorData {
    fn from(e: crate::errors::AgentflareError) -> Self {
        match e {
            crate::errors::AgentflareError::Backend(e) => map_backend_err(e),
            other => ErrorData::internal_error(other.to_string(), None),
        }
    }
}

/// 24 random bytes, hex-encoded — used as a webhook's HMAC signing secret
/// when the caller doesn't supply one.
fn generate_webhook_secret() -> String {
    use rand::Rng;
    let bytes: [u8; 24] = rand::thread_rng().r#gen();
    hex::encode(bytes)
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::engine::general_purpose;
    general_purpose::STANDARD.encode(bytes)
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ItemRequest {
    #[schemars(
        description = "Action: create|get|list|search|update|update_state|delete|claim|heartbeat|release|done|cancel|add_label|remove_label"
    )]
    action: String,
    #[schemars(
        description = "Item ID (required for get, update, update_state, delete, claim, heartbeat, release, done, add_label, remove_label)"
    )]
    #[serde(default)]
    id: Option<String>,
    #[schemars(description = "Item name/title (required for create)")]
    #[serde(default)]
    name: Option<String>,
    #[schemars(
        description = "State ID (create, update_state); omit to use the project's default (Backlog) state"
    )]
    #[serde(default)]
    state_id: Option<String>,
    #[schemars(description = "Markdown description body (create, update)")]
    #[serde(default)]
    description: Option<String>,
    #[schemars(description = "Priority: none|low|medium|high|urgent (create, update)")]
    #[serde(default)]
    priority: Option<String>,
    #[schemars(description = "Parent item ID, for sub-items (create)")]
    #[serde(default)]
    parent_id: Option<String>,
    #[schemars(
        description = "Agent ID to assign (create, update), or to filter by (list — matches items assigned to this agent plus unassigned ones, sorted open+assigned-to-you first)"
    )]
    #[serde(default)]
    assignee_agent: Option<String>,
    #[schemars(description = "Domain-specific fields as a JSON object (create)")]
    #[serde(default)]
    metadata: Option<serde_json::Value>,
    #[schemars(description = "Label IDs to attach on creation (create)")]
    #[serde(default)]
    label_ids: Option<Vec<String>>,
    #[schemars(description = "Item IDs this item depends on (create)")]
    #[serde(default)]
    dependency_ids: Option<Vec<String>>,
    #[schemars(description = "Label ID (add_label, remove_label)")]
    #[serde(default)]
    label_id: Option<String>,
    #[schemars(
        description = "Filter by state group (list); one of backlog|unstarted|started|completed|cancelled|triage, or a comma-separated list (e.g. \"backlog,unstarted,started\") to match any"
    )]
    #[serde(default)]
    state_group: Option<String>,
    #[schemars(
        description = "Max items to return (list: omit for no limit; search: omit for 20, capped at 1000)"
    )]
    #[serde(default)]
    limit: Option<i64>,
    #[schemars(description = "Items to skip before applying limit (list); default 0")]
    #[serde(default)]
    offset: Option<i64>,
    #[schemars(description = "FTS5 search query (search)")]
    #[serde(default)]
    query: Option<String>,
}

/// Lean per-item projection for `item(list)` — the raw 19-field `Item` (full
/// description/metadata/timestamps) is what `get` returns; `list` only needs
/// enough to triage, and resolves the opaque `state_id` into a readable name.
#[derive(Debug, serde::Serialize)]
struct ItemSummary {
    id: String,
    name: String,
    state: String,
    state_group: String,
    priority: String,
    assignee_agent: Option<String>,
    parent_id: Option<String>,
    sequence_id: i64,
    updated_at: i64,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct CommentRequest {
    #[schemars(description = "Action: create|edit|delete|list")]
    action: String,
    #[schemars(description = "Item ID to comment on (required for create, list)")]
    #[serde(default)]
    item_id: Option<String>,
    #[schemars(description = "Comment ID (required for edit, delete)")]
    #[serde(default)]
    id: Option<String>,
    #[schemars(description = "Comment body text (required for create, edit)")]
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct LabelRequest {
    #[schemars(description = "Action: create|list|update|delete")]
    action: String,
    #[schemars(description = "Label ID (required for update, delete)")]
    #[serde(default)]
    id: Option<String>,
    #[schemars(description = "Label name (required for create; optional for update)")]
    #[serde(default)]
    name: Option<String>,
    #[schemars(description = "Hex color, e.g. #F59E0B (create, update)")]
    #[serde(default)]
    color: Option<String>,
    #[schemars(description = "Parent label ID for nesting/grouping (create)")]
    #[serde(default)]
    parent_id: Option<String>,
    #[schemars(description = "Sort order for manual ordering (create, update)")]
    #[serde(default)]
    sort_order: Option<f64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct WebhookRequest {
    #[schemars(description = "Action: create|list|delete")]
    action: String,
    #[schemars(description = "Webhook ID (required for delete)")]
    #[serde(default)]
    id: Option<String>,
    #[schemars(description = "HTTPS/HTTP URL to deliver events to (required for create)")]
    #[serde(default)]
    url: Option<String>,
    #[schemars(description = "HMAC signing secret; auto-generated if omitted (create)")]
    #[serde(default)]
    secret: Option<String>,
    #[schemars(description = "Fire on item create/update/delete (create)")]
    #[serde(default)]
    on_item: Option<bool>,
    #[schemars(description = "Fire on state changes (create)")]
    #[serde(default)]
    on_state: Option<bool>,
    #[schemars(description = "Fire on project changes (create)")]
    #[serde(default)]
    on_project: Option<bool>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
struct ProjectRequest {
    #[schemars(description = "Action: info")]
    action: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct AssetRequest {
    #[schemars(description = "Action: attach|get|list|delete")]
    action: String,
    #[schemars(description = "Asset ID (required for get, delete)")]
    #[serde(default)]
    id: Option<String>,
    #[schemars(description = "Item ID to attach to (xor project_id)")]
    #[serde(default)]
    item_id: Option<String>,
    #[schemars(description = "Project ID to attach to (xor item_id)")]
    #[serde(default)]
    project_id: Option<String>,
    #[schemars(
        description = "Filename (required for attach) — must exist in ~/.agentflare/staging/"
    )]
    #[serde(default)]
    filename: Option<String>,
    #[schemars(description = "JSON metadata (optional, attach only)")]
    #[serde(default)]
    metadata: Option<String>,
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
        match req.action.as_str() {
            "publish" => {
                let name = req
                    .name
                    .ok_or_else(|| ErrorData::invalid_params("name is required", None))?;
                if name.trim().is_empty() {
                    return Err(ErrorData::invalid_params("name is required", None));
                }
                let content = req
                    .content
                    .ok_or_else(|| ErrorData::invalid_params("content is required", None))?;
                if content.is_empty() {
                    return Err(ErrorData::invalid_params("content is required", None));
                }
                let (store, base) = self.ensure_artifact_server()?;
                let req2 = agentflare_artifacts::PublishRequest {
                    name,
                    artifact_type: agentflare_artifacts::ArtifactType::from(
                        req.r#type.as_deref().unwrap_or("text"),
                    ),
                    content,
                    session_id: req.session_id.unwrap_or_default(),
                    update_id: req.update_id,
                    label: req.label,
                    description: req.description,
                    favicon: req.favicon,
                    base_version: req.base_version,
                    sender: self.agent.clone(),
                    recipient: req.recipient,
                    thread_id: req.thread_id,
                    reply_to: req.reply_to,
                    git: Self::git_provenance(),
                };
                let resp = store.publish(&req2).map_err(Self::artifact_error)?;
                Ok(serde_json::to_string_pretty(&serde_json::json!({ "id": resp.id, "version": resp.version, "url": format!("{base}/{}", resp.id), "index": format!("{base}/") })).unwrap_or_default())
            }
            "list" => {
                let (store, base) = self.ensure_artifact_server()?;
                let summaries = store
                    .list(req.session_id.as_deref())
                    .map_err(Self::artifact_error)?;
                let items: Vec<serde_json::Value> = summaries
                    .iter()
                    .filter(|s| {
                        req.inbox_recipient
                            .as_deref()
                            .is_none_or(|r| s.recipient.as_deref() == Some(r))
                            && req
                                .thread_id
                                .as_deref()
                                .is_none_or(|t| s.thread_id.as_deref() == Some(t))
                    })
                    .map(|s| {
                        let mut v = serde_json::to_value(s).unwrap_or_default();
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("url".into(), serde_json::json!(format!("{base}/{}", s.id)));
                        }
                        v
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&items).unwrap_or_default())
            }
            "get" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required", None))?;
                let (store, _) = self.ensure_artifact_server()?;
                let artifact = match req.version {
                    Some(n) => store.get_version(&id, n),
                    None => store.get(&id),
                }
                .map_err(Self::artifact_error)?;
                Ok(serde_json::to_string_pretty(&artifact).unwrap_or_default())
            }
            "diff" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required", None))?;
                let from_version = req
                    .from_version
                    .ok_or_else(|| ErrorData::invalid_params("from_version is required", None))?;
                let (store, _) = self.ensure_artifact_server()?;
                let to = match req.to_version {
                    Some(v) => v,
                    None => store.get(&id).map_err(Self::artifact_error)?.version,
                };
                let diff = store
                    .diff(&id, from_version, to)
                    .map_err(Self::artifact_error)?;
                Ok(serde_json::to_string_pretty(&diff).unwrap_or_default())
            }
            "search" => {
                let query = req
                    .query
                    .ok_or_else(|| ErrorData::invalid_params("query is required", None))?;
                if query.trim().is_empty() {
                    return Err(ErrorData::invalid_params("query is required", None));
                }
                let (store, base) = self.ensure_artifact_server()?;
                let needle = query.to_lowercase();
                let mut hits = Vec::new();
                for summary in store
                    .list(req.session_id.as_deref())
                    .map_err(Self::artifact_error)?
                {
                    let name_hit = summary.name.to_lowercase().contains(&needle);
                    let desc_hit = summary
                        .description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&needle));
                    let content = store
                        .get(&summary.id)
                        .map(|a| a.content)
                        .unwrap_or_default();
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
            "delete" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required", None))?;
                let (store, _) = self.ensure_artifact_server()?;
                store.delete(&id).map_err(Self::artifact_error)?;
                Ok(serde_json::json!({"deleted": id}).to_string())
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }
    #[tool(
        description = "Hand a work product to another agent: assigns/creates an item for the recipient (in the repo's linked project) and attaches the content to it as an asset. Re-attaching under the same item_id creates the next asset version, not a duplicate. Sender is this runtime's own identity."
    )]
    fn handoff(
        &self,
        Parameters(HandoffRequest {
            recipient,
            name,
            content,
            r#type,
            item_id,
            thread_id,
            reply_to,
            description,
        }): Parameters<HandoffRequest>,
    ) -> Result<String, ErrorData> {
        if recipient.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "recipient is required for a handoff — without it the item lands with no assignee",
                None,
            ));
        }
        if name.trim().is_empty() {
            return Err(ErrorData::invalid_params("name is required", None));
        }
        if content.is_empty() {
            return Err(ErrorData::invalid_params("content is required", None));
        }
        let recipient = recipient.trim().to_string();
        let name = name.trim().to_string();
        let ext = match r#type.as_deref() {
            Some("html") => "html",
            Some("mermaid") | Some("diagram") => "mmd",
            Some("text") => "txt",
            _ => "md",
        };

        self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            let ws_id = Self::resolve_workspace_id(conn)?;

            let item = match &item_id {
                Some(id) => {
                    let input = agentflare_backend::item::UpdateItem {
                        assignee_agent: Some(recipient.clone()),
                        ..Default::default()
                    };
                    agentflare_backend::item::update(conn, id, input).map_err(map_backend_err)?
                }
                None => {
                    let state_id = agentflare_backend::state::list_by_project(conn, &project.id)
                        .map_err(map_backend_err)?
                        .into_iter()
                        .find(|s| s.is_default)
                        .ok_or_else(|| {
                            ErrorData::internal_error("project has no default state", None)
                        })?
                        .id;
                    let metadata = thread_id
                        .as_ref()
                        .map(|t| serde_json::json!({ "thread": t }).to_string());
                    let input = agentflare_backend::item::CreateItem {
                        project_id: project.id.clone(),
                        state_id,
                        name: name.clone(),
                        description: description.clone().or_else(|| Some(content.clone())),
                        priority: None,
                        parent_id: None,
                        assignee_agent: Some(recipient.clone()),
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                        metadata,
                        label_ids: vec![],
                        assignee_ids: vec![],
                        dependency_ids: vec![],
                    };
                    agentflare_backend::item::create(conn, input).map_err(map_backend_err)?
                }
            };

            let bytes = content.as_bytes();
            let hash = Self::content_hash(bytes);
            // Keyed on item.id, not name — name is the per-call brief and
            // can legitimately differ between messages on the same item
            // (e.g. a reply's brief vs. the original ask); keying on it
            // would silently reset versioning to 1 instead of continuing
            // the chain.
            let safe_stem = Self::slugify(&item.id);
            let filename = format!("{safe_stem}.{ext}");
            let full_storage = format!("{ws_id}/assets/{safe_stem}-{hash}.{ext}");
            let base_path = crate::paths::home().join(".agentflare");
            let target = base_path.join(&full_storage);
            if !target.exists() {
                agentflare_backend::asset::write_file(&base_path, &full_storage, bytes)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            }
            let mut meta = serde_json::json!({ "sender": self.agent, "recipient": recipient });
            if let Some(t) = &thread_id {
                meta["thread_id"] = serde_json::json!(t);
            }
            if let Some(r) = &reply_to {
                meta["reply_to"] = serde_json::json!(r);
            }
            let asset = agentflare_backend::asset::create(
                conn,
                agentflare_backend::asset::CreateAsset {
                    workspace_id: Some(ws_id),
                    entity_type: "item_attachment".into(),
                    entity_id: item.id.clone(),
                    filename,
                    size: bytes.len() as i64,
                    mime_type: Some(Self::infer_mime_type(ext)),
                    metadata: Some(meta.to_string()),
                    storage_path: Some(full_storage),
                },
            )
            .map_err(map_backend_err)?;

            let result = serde_json::json!({
                "item_id": item.id,
                "item_sequence_id": item.sequence_id,
                "asset_id": asset.id,
                "asset_version": asset.version,
                "recipient": recipient,
            });
            Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
        })?
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
    fn repo_root() -> std::path::PathBuf {
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(root) = crate::git::repo_toplevel(&cwd) {
            return root;
        }
        Self::find_root_from(&cwd, &crate::paths::home())
    }

    /// `repo_root()`, but honoring `worktree_repo_root_override` — used only
    /// by the worktree-on-claim feature so tests never run real `git
    /// worktree`/branch operations against this actual repository.
    fn worktree_repo_root(&self) -> std::path::PathBuf {
        self.worktree_repo_root_override
            .clone()
            .unwrap_or_else(Self::repo_root)
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
    fn with_backend_db<T>(
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
        match req.action.as_str() {
            "acquire" => {
                let target = req
                    .target
                    .ok_or_else(|| ErrorData::invalid_params("target is required", None))?;
                let repo_opt = req.repo;
                let repo_overridden = repo_opt.as_ref().is_some_and(|r| !r.is_empty());
                let (conn, repo) = Self::claim_ctx(&target, repo_opt)?;
                let owner = crate::claims::owner_id();
                let commit = if repo_overridden {
                    None
                } else {
                    Self::git_provenance().and_then(|g| g.commit)
                };
                let outcome = crate::claims::acquire(
                    &conn,
                    &repo,
                    &target,
                    &owner,
                    commit.as_deref(),
                    crate::claims::now(),
                    crate::claims::ttl_secs(),
                )
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(match outcome {
                    crate::claims::Acquire::Acquired => serde_json::json!({ "status": "acquired", "repo": repo, "target": target, "owner": owner }),
                    crate::claims::Acquire::Held { owner: holder, age_secs } => serde_json::json!({ "status": "held", "repo": repo, "target": target, "owner": holder, "age_secs": age_secs }),
                }.to_string())
            }
            "heartbeat" => {
                let target = req
                    .target
                    .ok_or_else(|| ErrorData::invalid_params("target is required", None))?;
                let (conn, repo) = Self::claim_ctx(&target, req.repo)?;
                let owner = crate::claims::owner_id();
                let ok =
                    crate::claims::heartbeat(&conn, &repo, &target, &owner, crate::claims::now())
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(
                    serde_json::json!({ "refreshed": ok, "repo": repo, "target": target })
                        .to_string(),
                )
            }
            "release" => {
                let target = req
                    .target
                    .ok_or_else(|| ErrorData::invalid_params("target is required", None))?;
                let (conn, repo) = Self::claim_ctx(&target, req.repo)?;
                let owner = crate::claims::owner_id();
                let ok = crate::claims::release(&conn, &repo, &target, &owner)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(
                    serde_json::json!({ "released": ok, "repo": repo, "target": target })
                        .to_string(),
                )
            }
            "done" => {
                let target = req
                    .target
                    .ok_or_else(|| ErrorData::invalid_params("target is required", None))?;
                let (conn, repo) = Self::claim_ctx(&target, req.repo)?;
                let owner = crate::claims::owner_id();
                let ok = crate::claims::done(&conn, &repo, &target, &owner, crate::claims::now())
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(serde_json::json!({ "done": ok, "repo": repo, "target": target }).to_string())
            }
            "list" => {
                let conn = Self::claim_db()?;
                let scope = if req.all_repos {
                    None
                } else {
                    Some(crate::claims::resolve_repo(req.repo).ok_or_else(|| ErrorData::invalid_params("could not determine repo — run in a git repo or pass repo=owner/name (or all_repos=true)", None))?)
                };
                let claims = crate::claims::list(
                    &conn,
                    scope.as_deref(),
                    req.all,
                    crate::claims::now(),
                    crate::claims::ttl_secs(),
                )
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(serde_json::to_string_pretty(&claims).unwrap_or_default())
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
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
        match req.action.as_str() {
            "submit" => {
                let findings = req
                    .findings
                    .ok_or_else(|| ErrorData::invalid_params("findings is required", None))?;
                let conn = Self::claim_db()?;
                let repo = Self::resolve_repo_or_err(req.repo)?;
                let pr = Self::resolve_round(req.pr)?;
                // SECURITY / step-3 classification (#75): the finder `agent`
                // stays caller-settable BY DESIGN. Unlike artifact authorship,
                // review findings live in a local, per-repo, single-user DB, and
                // a `/code-review` orchestrator legitimately submits on behalf
                // of many finder sub-agents — consensus counts DISTINCT finder
                // names, so collapsing them to one server identity would break
                // it. No cross-principal trust boundary exists here; the
                // server-derived `submitter_name` is the fallback when unset.
                let agent = req
                    .agent
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(crate::review::submitter_name);
                let parsed: Vec<crate::review::Finding> = findings
                    .into_iter()
                    .map(serde_json::from_value)
                    .collect::<Result<_, _>>()
                    .map_err(|e| {
                        ErrorData::invalid_params(format!("invalid finding: {e}"), None)
                    })?;
                let n =
                    crate::review::submit(&conn, &repo, &pr, &agent, &parsed, crate::claims::now())
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(
                    serde_json::json!({ "submitted": n, "repo": repo, "pr": pr, "agent": agent })
                        .to_string(),
                )
            }
            "consensus" => {
                let conn = Self::claim_db()?;
                let repo = Self::resolve_repo_or_err(req.repo)?;
                let pr = Self::resolve_round(req.pr)?;
                let findings = crate::review::load(&conn, &repo, &pr)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let diff = crate::review::compute_diff(req.base.as_deref(), req.head.as_deref())
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let changed = crate::review::changed_lines(&diff);
                let result = crate::review::consensus(&findings, &changed);
                Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
            }
            "list" => {
                let conn = Self::claim_db()?;
                let repo = Self::resolve_repo_or_err(req.repo)?;
                let pr = Self::resolve_round(req.pr)?;
                let findings = crate::review::load(&conn, &repo, &pr)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let rows: Vec<serde_json::Value> = findings.iter().map(|sf| serde_json::json!({ "agent": sf.agent, "file": sf.finding.file, "line": sf.finding.line, "message": sf.finding.message, "severity": sf.finding.severity })).collect();
                Ok(serde_json::to_string_pretty(&rows).unwrap_or_default())
            }
            "clear" => {
                let conn = Self::claim_db()?;
                let repo = Self::resolve_repo_or_err(req.repo)?;
                let pr = Self::resolve_round(req.pr)?;
                crate::review::clear(&conn, &repo, &pr)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(serde_json::json!({"cleared": true}).to_string())
            }
            "record" => {
                let conn = Self::claim_db()?;
                let repo = Self::resolve_repo_or_err(req.repo)?;
                let pr = Self::resolve_round(req.pr)?;
                let findings = crate::review::load(&conn, &repo, &pr)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let diff = crate::review::compute_diff(req.base.as_deref(), req.head.as_deref())
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                let changed = crate::review::changed_lines(&diff);
                let n = crate::review::record_round(
                    &conn,
                    &repo,
                    &pr,
                    &findings,
                    &changed,
                    crate::claims::now(),
                )
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(serde_json::json!({ "recorded_agents": n, "repo": repo, "pr": pr }).to_string())
            }
            "scores" => {
                let conn = Self::claim_db()?;
                let repo = req.repo;
                let all_repos = req.all_repos;
                let scope = if all_repos {
                    None
                } else {
                    Some(Self::resolve_repo_or_err(repo)?)
                };
                let scores = crate::review::scores(&conn, scope.as_deref())
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                Ok(serde_json::to_string_pretty(&scores).unwrap_or_default())
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
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
        match req.action.as_str() {
            "remember" => {
                let title = req
                    .title
                    .ok_or_else(|| ErrorData::invalid_params("title is required", None))?;
                let content = req
                    .content
                    .ok_or_else(|| ErrorData::invalid_params("content is required", None))?;
                let r#type = req
                    .r#type
                    .ok_or_else(|| ErrorData::invalid_params("type is required", None))?;
                let input = crate::memory::mcp::RememberInput {
                    title,
                    content,
                    r#type,
                    session_id: req.session_id,
                    project: req.project,
                    topic_key: req.topic_key,
                    scope: req.scope,
                };
                crate::memory::mcp::handle_remember(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "recall" => {
                let input = crate::memory::mcp::RecallInput {
                    query: req.query,
                    id: req.id,
                    r#type: req.r#type,
                    project: req.project,
                    limit: req.limit,
                };
                crate::memory::mcp::handle_recall(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "context" => {
                let input = crate::memory::mcp::ContextInput {
                    session_id: req.session_id,
                    project: req.project,
                };
                crate::memory::mcp::handle_context(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "handoff" => {
                let session_id = req
                    .session_id
                    .ok_or_else(|| ErrorData::invalid_params("session_id is required", None))?;
                let summary = req
                    .summary
                    .ok_or_else(|| ErrorData::invalid_params("summary is required", None))?;
                let input = crate::memory::mcp::HandoffInput {
                    session_id,
                    summary,
                    findings: req.findings,
                    decisions: req.decisions,
                    files_touched: req.files_touched,
                    evidence: req.evidence,
                };
                crate::memory::mcp::handle_handoff(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "compact" => {
                let input = crate::memory::mcp::CompactInput {
                    lines: req.content.unwrap_or_default(),
                    query: req.query,
                    compression_ratio: req.compression_ratio,
                    preserve_recent: req.preserve_recent,
                    scorer: req.scorer,
                };
                crate::memory::mcp::handle_compact(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "relate" => {
                let source_id = req
                    .source_id
                    .ok_or_else(|| ErrorData::invalid_params("source_id is required", None))?;
                let target_id = req
                    .target_id
                    .ok_or_else(|| ErrorData::invalid_params("target_id is required", None))?;
                let relation = req
                    .relation
                    .ok_or_else(|| ErrorData::invalid_params("relation is required", None))?;
                let input = crate::memory::mcp::RelateInput {
                    source_id,
                    target_id,
                    relation,
                    reason: req.reason,
                    confidence: req.confidence,
                };
                crate::memory::mcp::handle_relate(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "curate" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required", None))?;
                let curate_action = req.curate_action.ok_or_else(|| {
                    ErrorData::invalid_params(
                        "curate_action is required (update|delete|pin|unpin)",
                        None,
                    )
                })?;
                let input = crate::memory::mcp::CurateInput {
                    action: curate_action,
                    id,
                    title: req.title,
                    content: req.content,
                    r#type: req.r#type,
                    pinned: req.pinned,
                };
                crate::memory::mcp::handle_curate(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
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
            other => Err(ErrorData::invalid_params(
                format!(
                    "unknown item action: '{other}' — expected create|get|list|search|update|update_state|delete|claim|heartbeat|release|done|cancel|add_label|remove_label"
                ),
                None,
            )),
        }
    }

    #[tool(
        description = "Manage work items in the repo's linked project. Single consolidated tool with `action` field (create|get|list|search|update|update_state|delete|claim|heartbeat|release|done|cancel|add_label|remove_label). See each field's description for when it's required."
    )]
    fn item(&self, Parameters(req): Parameters<ItemRequest>) -> Result<String, ErrorData> {
        self.item_inner(req)
    }

    #[tool(
        description = "Create, edit, delete, or list threaded comments on an item. Single consolidated tool with `action` field (create|edit|delete|list). Only the author of a comment may edit/delete it, only the latest comment on an item is editable/deletable, and edit/delete are blocked while another agent holds an active claim on the item."
    )]
    fn comment(&self, Parameters(req): Parameters<CommentRequest>) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "create" => {
                let item_id = req.item_id.ok_or_else(|| {
                    ErrorData::invalid_params("item_id is required for create", None)
                })?;
                let body = req.body.ok_or_else(|| {
                    ErrorData::invalid_params("body is required for create", None)
                })?;
                if item_id.trim().is_empty() || body.trim().is_empty() {
                    return Err(ErrorData::invalid_params(
                        "item_id and body are required",
                        None,
                    ));
                }
                let author = crate::claims::owner_id();
                self.with_backend_db(|conn| {
                    let comment =
                        agentflare_backend::comment::create(conn, &item_id, &author, &body)
                            .map_err(map_backend_err)?;
                    Ok(serde_json::to_string_pretty(&comment).unwrap_or_default())
                })?
            }
            "edit" => {
                let comment_id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required for edit", None))?;
                let body = req
                    .body
                    .ok_or_else(|| ErrorData::invalid_params("body is required for edit", None))?;
                if comment_id.trim().is_empty() || body.trim().is_empty() {
                    return Err(ErrorData::invalid_params("id and body are required", None));
                }
                let owner = crate::claims::owner_id();
                let now = crate::claims::now();
                let ttl = backend_claim_ttl_secs();
                self.with_backend_db(|conn| {
                    // The author/latest/claim checks and the write must be one
                    // transaction — otherwise a comment landing between the
                    // is_latest check and the write (routine under concurrent
                    // multi-agent access) can silently violate the
                    // "only the latest comment is editable" invariant.
                    let tx = conn
                        .unchecked_transaction()
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    let comment = agentflare_backend::comment::get(&tx, &comment_id)
                        .map_err(map_backend_err)?;
                    if crate::claims::agent_of(&comment.author_agent)
                        != crate::claims::agent_of(&owner)
                    {
                        return Err(ErrorData::invalid_params(
                            "can only edit your own comments",
                            None,
                        ));
                    }
                    if !agentflare_backend::comment::is_latest(&tx, &comment)
                        .map_err(map_backend_err)?
                    {
                        return Err(ErrorData::invalid_params(
                            "comment is not the latest on this item — cannot edit",
                            None,
                        ));
                    }
                    if agentflare_backend::claim::has_active_claim_by_other(
                        &tx,
                        &comment.item_id,
                        &owner,
                        now,
                        ttl,
                    )
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    {
                        return Err(ErrorData::invalid_params(
                            "another agent has started work on this item — cannot edit",
                            None,
                        ));
                    }
                    let updated = agentflare_backend::comment::update(&tx, &comment_id, &body)
                        .map_err(map_backend_err)?;
                    tx.commit()
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    Ok(serde_json::to_string_pretty(&updated).unwrap_or_default())
                })?
            }
            "delete" => {
                let comment_id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required for delete", None))?;
                if comment_id.trim().is_empty() {
                    return Err(ErrorData::invalid_params("id is required", None));
                }
                let owner = crate::claims::owner_id();
                let now = crate::claims::now();
                let ttl = backend_claim_ttl_secs();
                self.with_backend_db(|conn| {
                    // See "edit" above: checks + write must be one transaction
                    // to close the same TOCTOU window.
                    let tx = conn
                        .unchecked_transaction()
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    let comment = agentflare_backend::comment::get(&tx, &comment_id)
                        .map_err(map_backend_err)?;
                    if crate::claims::agent_of(&comment.author_agent)
                        != crate::claims::agent_of(&owner)
                    {
                        return Err(ErrorData::invalid_params(
                            "can only delete your own comments",
                            None,
                        ));
                    }
                    if !agentflare_backend::comment::is_latest(&tx, &comment)
                        .map_err(map_backend_err)?
                    {
                        return Err(ErrorData::invalid_params(
                            "comment is not the latest on this item — cannot delete",
                            None,
                        ));
                    }
                    if agentflare_backend::claim::has_active_claim_by_other(
                        &tx,
                        &comment.item_id,
                        &owner,
                        now,
                        ttl,
                    )
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    {
                        return Err(ErrorData::invalid_params(
                            "another agent has started work on this item — cannot delete",
                            None,
                        ));
                    }
                    agentflare_backend::comment::delete(&tx, &comment_id)
                        .map_err(map_backend_err)?;
                    tx.commit()
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    Ok(serde_json::json!({"deleted": true, "id": comment_id}).to_string())
                })?
            }
            "list" => {
                let item_id = req.item_id.ok_or_else(|| {
                    ErrorData::invalid_params("item_id is required for list", None)
                })?;
                if item_id.trim().is_empty() {
                    return Err(ErrorData::invalid_params("item_id is required", None));
                }
                self.with_backend_db(|conn| {
                    let comments = agentflare_backend::comment::list_by_item(conn, &item_id)
                        .map_err(map_backend_err)?;
                    Ok(serde_json::to_string_pretty(&comments).unwrap_or_default())
                })?
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown comment action: '{other}' — expected create|edit|delete|list"),
                None,
            )),
        }
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
    fn asset(
        &self,
        Parameters(AssetRequest {
            action,
            id,
            item_id,
            project_id,
            filename,
            metadata,
        }): Parameters<AssetRequest>,
    ) -> Result<String, ErrorData> {
        match action.as_str() {
            "attach" => {
                let has_item = item_id.is_some();
                let has_project = project_id.is_some();
                if has_item == has_project {
                    return Err(ErrorData::invalid_params(
                        "exactly one of item_id or project_id is required for attach",
                        None,
                    ));
                }
                let fn_val = filename.ok_or_else(|| {
                    ErrorData::invalid_params("filename is required for attach", None)
                })?;
                // path traversal guard: reject filename with .. or absolute components
                let staged_rel = std::path::Path::new(&fn_val);
                if staged_rel
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "filename '{fn_val}' contains path separators or parent-refs — not allowed"
                        ),
                        None,
                    ));
                }
                let staging_dir = crate::paths::home().join(".agentflare").join("staging");
                let staged = staging_dir.join(&fn_val);
                if !staged.exists() {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "file not found at staging path: {} — write the file there before calling attach",
                            staged.display()
                        ),
                        None,
                    ));
                }
                let size = std::fs::metadata(&staged)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                    .len();
                let max_attach = Self::asset_max_attach_bytes();
                if size > max_attach {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "file is {} bytes, exceeds the {} byte attach limit",
                            size, max_attach
                        ),
                        None,
                    ));
                }
                let bytes = std::fs::read(&staged)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let hash = Self::content_hash(&bytes);
                let meta = metadata.unwrap_or_else(|| "{}".to_string());
                self.with_backend_db(|conn| {
                    let ws_id = Self::resolve_workspace_id(conn)?;
                    let (entity_type, entity_id) = if has_item {
                        agentflare_backend::item::get(conn, item_id.as_ref().unwrap())
                            .map_err(map_backend_err)?;
                        ("item_attachment", item_id.as_ref().unwrap().clone())
                    } else {
                        agentflare_backend::project::get(conn, project_id.as_ref().unwrap())
                            .map_err(map_backend_err)?;
                        ("project_attachment", project_id.as_ref().unwrap().clone())
                    };
                    let ext = std::path::Path::new(&fn_val)
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("");
                    let mime = Self::infer_mime_type(ext);
                    let stem = std::path::Path::new(&fn_val)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or(&fn_val);
                    let safe_stem: String = {
                        let s: String = stem
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                            .collect();
                        if s.is_empty() { "file".to_string() } else { s }
                    };
                    let full_storage = if ext.is_empty() {
                        format!("{}/assets/{}-{}", ws_id, safe_stem, hash)
                    } else {
                        format!("{}/assets/{}-{}.{}", ws_id, safe_stem, hash, ext)
                    };
                    let base_path = crate::paths::home().join(".agentflare");
                    // only write if file doesn't already exist (same content already stored)
                    let target = base_path.join(&full_storage);
                    if !target.exists() {
                        agentflare_backend::asset::write_file(&base_path, &full_storage, &bytes)
                            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    }
                    let asset = agentflare_backend::asset::create(
                        conn,
                        agentflare_backend::asset::CreateAsset {
                            workspace_id: Some(ws_id.clone()),
                            entity_type: entity_type.into(),
                            entity_id,
                            filename: fn_val.clone(),
                            size: size as i64,
                            mime_type: Some(mime),
                            metadata: Some(meta),
                            storage_path: Some(full_storage),
                        },
                    )
                    .map_err(map_backend_err)?;
                    // remove staging file only after the DB insert succeeds
                    let _ = std::fs::remove_file(&staged);
                    Ok(
                        serde_json::to_string_pretty(&Self::strip_storage_path(&asset))
                            .unwrap_or_default(),
                    )
                })?
            }
            "get" => {
                let id =
                    id.ok_or_else(|| ErrorData::invalid_params("id is required for get", None))?;
                self.with_backend_db(|conn| {
                    let asset = agentflare_backend::asset::get(conn, &id)
                        .map_err(map_backend_err)?;
                    let base_path = crate::paths::home().join(".agentflare");
                    let max_inline = Self::asset_max_inline_bytes();
                    let meta = Self::strip_storage_path(&asset);
                    let size = asset.size as u64;
                    if size <= max_inline {
                        match agentflare_backend::asset::read_file(&base_path, &asset.storage_path) {
                            Ok(bytes) => {
                                // Textual MIME + valid UTF-8 => return readable text so
                                // callers don't decode every text asset; everything else
                                // (binary MIME, or invalid UTF-8) => Base64.
                                let (content, encoding) = match std::str::from_utf8(&bytes) {
                                    Ok(text) if Self::mime_is_textual(asset.mime_type.as_deref()) => (text.to_string(), "utf8"),
                                    _ => (base64_encode(&bytes), "base64"),
                                };
                                let result = serde_json::json!({
                                    "asset": meta,
                                    "content": content,
                                    "encoding": encoding,
                                });
                                Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
                            }
                            Err(e) => {
                                let result = serde_json::json!({
                                    "asset": meta,
                                    "content": null,
                                    "content_omitted_reason": format!("could not read file: {}", e),
                                });
                                Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
                            }
                        }
                    } else {
                        let result = serde_json::json!({
                            "asset": meta,
                            "content": null,
                            "content_omitted_reason": format!("file is {} bytes, exceeds the {} byte inline limit", size, max_inline),
                        });
                        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
                    }
                })?
            }
            "list" => self.with_backend_db(|conn| {
                let ws_id = Self::resolve_workspace_id(conn)?;
                let assets: Vec<agentflare_backend::asset::Asset> = match (item_id, project_id) {
                    (Some(iid), None) => {
                        agentflare_backend::asset::list_by_entity(conn, "item_attachment", &iid)
                            .map_err(map_backend_err)?
                    }
                    (None, Some(pid)) => {
                        agentflare_backend::asset::list_by_entity(conn, "project_attachment", &pid)
                            .map_err(map_backend_err)?
                    }
                    (Some(_), Some(_)) => {
                        return Err(ErrorData::invalid_params(
                            "only one of item_id or project_id allowed for list, not both",
                            None,
                        ));
                    }
                    (None, None) => {
                        let mut assets: Vec<serde_json::Value> = Vec::new();
                        for a in agentflare_backend::asset::list_by_workspace(conn, &ws_id)
                            .map_err(map_backend_err)?
                        {
                            assets.push(Self::strip_storage_path(&a));
                        }
                        return Ok(serde_json::to_string_pretty(&assets).unwrap_or_default());
                    }
                };
                let mut stripped: Vec<serde_json::Value> = Vec::new();
                for a in assets {
                    stripped.push(Self::strip_storage_path(&a));
                }
                Ok(serde_json::to_string_pretty(&stripped).unwrap_or_default())
            })?,
            "delete" => {
                let id =
                    id.ok_or_else(|| ErrorData::invalid_params("id is required for delete", None))?;
                self.with_backend_db(|conn| {
                    let asset = agentflare_backend::asset::get(conn, &id)
                        .map_err(map_backend_err)?;
                    // soft-delete the row
                    agentflare_backend::asset::delete(conn, &id)
                        .map_err(map_backend_err)?;
                    // only unlink from disk if no other live row references the same storage_path
                    let remaining: i64 = conn
                        .query_row(
                            "SELECT count(*) FROM assets WHERE storage_path = ?1 AND deleted_at IS NULL",
                            rusqlite::params![&asset.storage_path],
                            |r| r.get(0),
                        )
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                    if remaining == 0 {
                        let base_path = crate::paths::home().join(".agentflare");
                        let _ = agentflare_backend::asset::delete_file(&base_path, &asset.storage_path);
                    }
                    Ok(serde_json::json!({"deleted": true, "id": id}).to_string())
                })?
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action '{other}'; expected attach|get|list|delete"),
                None,
            )),
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
mod tests {
    use super::*;

    #[test]
    fn parse_flared_port_reads_top_level_key_only() {
        assert_eq!(parse_flared_port("port = 4444\n"), Some(4444));
        assert_eq!(
            parse_flared_port("# comment\nport=9999 # inline\n"),
            Some(9999)
        );
        // tables end the top-level scan; a port inside one is not flared's
        assert_eq!(parse_flared_port("[[registries]]\nport = 1\n"), None);
        // prefix collisions and malformed values are not overrides
        assert_eq!(
            parse_flared_port("portable = 1\nlight_interval_secs = 60\n"),
            None
        );
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

    #[test]
    fn optimize_tool_retrieve_returns_registered_original() {
        crate::paths::test_support::with_temp_home(|| {
            let backup = crate::state::state_dir().join("o.md");
            std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
            std::fs::write(&backup, "ORIG").unwrap();
            let e = crate::optimize::retrieve::register(
                crate::optimize::retrieve::EntryKind::FileBackup {
                    backup_path: backup,
                },
                4,
                1,
                1,
            );

            let s = AgentflareMcp::default();
            let out = s
                .optimize(Parameters(OptimizeRequest {
                    action: "retrieve".into(),
                    id: Some(e.id),
                }))
                .unwrap();
            assert_eq!(out, "ORIG");
        });
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

    #[tokio::test]
    async fn skill_search_empty_query_is_invalid_params() {
        let s = AgentflareMcp::default();
        let err = s
            .skill(Parameters(SkillRequest {
                action: "search".into(),
                query: Some("".into()),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("query"));
    }

    #[tokio::test]
    async fn skill_load_unknown_name_reports_not_found_with_search_hint() {
        // Isolated DB path so the test never opens/refreshes the shared skills.db.
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            skills_db_override: Some(tmp.path().join("skills.db")),
            ..Default::default()
        };
        let out = s
            .skill(Parameters(SkillRequest {
                action: "load".into(),
                name: Some("definitely-not-a-skill-xyz".into()),
                original: false,
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert!(out.to_string().contains("skill_search"));
    }

    #[tokio::test]
    async fn skill_search_mode_rejects_unknown_value() {
        let s = AgentflareMcp::default();
        let err = s
            .skill(Parameters(SkillRequest {
                action: "search".into(),
                query: Some("anything".into()),
                mode: Some("fuzzy".into()),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("mode"));
    }

    #[tokio::test]
    async fn tool_search_empty_query_is_invalid_params() {
        // Isolated DB path so the test never opens/refreshes the shared gateway.db.
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            gateway_db_override: Some(tmp.path().join("gateway.db")),
            ..Default::default()
        };
        let err = s
            .tool(Parameters(ToolRequest {
                action: "search".into(),
                query: Some("".into()),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("query is required"));
    }

    #[tokio::test]
    async fn tool_search_mode_rejects_unknown_value() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            gateway_db_override: Some(tmp.path().join("gateway.db")),
            ..Default::default()
        };
        let err = s
            .tool(Parameters(ToolRequest {
                action: "search".into(),
                query: Some("x".into()),
                mode: Some("bogus".into()),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("mode must be"));
    }

    #[tokio::test]
    async fn tool_execute_requires_server_and_tool() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            gateway_db_override: Some(tmp.path().join("gateway.db")),
            ..Default::default()
        };
        let err = s
            .tool(Parameters(ToolRequest {
                action: "execute".into(),
                server: Some("".into()),
                tool: Some("x".into()),
                args: Some(serde_json::Map::new()),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("required"));
    }

    #[tokio::test]
    async fn tool_execute_unknown_server_is_invalid_params() {
        // Isolated DB path, no servers configured — `Registry::execute` is
        // guaranteed to hit `GatewayError::ServerNotFound`, which must map to
        // `invalid_params` (a caller-fixable mistake), not `internal_error`.
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            gateway_db_override: Some(tmp.path().join("gateway.db")),
            ..Default::default()
        };
        let err = s
            .tool(Parameters(ToolRequest {
                action: "execute".into(),
                server: Some("definitely-not-a-configured-server".into()),
                tool: Some("x".into()),
                args: Some(serde_json::Map::new()),
                ..Default::default()
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn tool_execute_args_schema_is_object_or_null() {
        let schema = schemars::schema_for!(ToolRequest);
        let schema_json = serde_json::to_value(&schema).unwrap();
        let args_schema = schema_json
            .get("properties")
            .and_then(|p| p.get("args"))
            .expect("args schema present");
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
            .artifact(Parameters(ArtifactRequest {
                action: "publish".into(),
                name: Some("hello".into()),
                r#type: None,
                content: Some("artifact-body-marker".into()),
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
        assert!(
            resp.contains("artifact-body-marker"),
            "body present: {resp}"
        );
    }

    #[test]
    fn artifact_publish_update_id_keeps_same_id() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let first: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "publish".into(),
                name: Some("doc".into()),
                r#type: Some("markdown".into()),
                content: Some("v1".into()),
                session_id: Some("ses-1".into()),
                update_id: None,
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let id = first["id"].as_str().unwrap().to_string();

        let second: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "publish".into(),
                name: Some("doc".into()),
                r#type: Some("markdown".into()),
                content: Some("v2".into()),
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
                &s.artifact(Parameters(ArtifactRequest {
                    action: "publish".into(),
                    name: Some(name.into()),
                    r#type: None,
                    content: Some(format!("content-of-{name}")),
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

        let all: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "list".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);

        let one: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "list".into(),
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
            &s.artifact(Parameters(ArtifactRequest {
                action: "get".into(),
                id: Some(id.clone()),
                version: None,
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(got["content"], "content-of-alpha");

        let del: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "delete".into(),
                id: Some(id.clone()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(del["deleted"], id);

        let err = s
            .artifact(Parameters(ArtifactRequest {
                action: "get".into(),
                id: Some(id),
                version: None,
                ..Default::default()
            }))
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
            &s.artifact(Parameters(ArtifactRequest {
                action: "publish".into(),
                name: Some("doc".into()),
                r#type: None,
                content: Some("v1".into()),
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
            s.artifact(Parameters(ArtifactRequest {
                action: "publish".into(),
                name: Some("doc".into()),
                r#type: None,
                content: Some(content.into()),
                session_id: None,
                update_id: Some(id.clone()),
                base_version: base,
                ..Default::default()
            }))
        };
        let second: serde_json::Value =
            serde_json::from_str(&update(Some(1), "v2").unwrap()).unwrap();
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
        let publish =
            |name: &str, recipient: Option<&str>, thread: Option<&str>| -> serde_json::Value {
                serde_json::from_str(
                    &s.artifact(Parameters(ArtifactRequest {
                        action: "publish".into(),
                        name: Some(name.into()),
                        content: Some(format!("content {name}")),
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
            &s.artifact(Parameters(ArtifactRequest {
                action: "list".into(),
                inbox_recipient: Some("codex".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(inbox.as_array().unwrap().len(), 1);
        assert_eq!(inbox[0]["name"], "packet");

        let thread: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "list".into(),
                thread_id: Some("t1".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(thread.as_array().unwrap().len(), 2);
    }

    fn handoff_harness() -> (tempfile::TempDir, AgentflareMcp) {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            backend_db_override: Some(tmp.path().join("backend.db")),
            backend_project_link_override: Some(tmp.path().join("project.json")),
            agent: Some("claude-code".into()),
            ..Default::default()
        };
        (tmp, s)
    }

    fn item_assets(s: &AgentflareMcp, item_id: &str) -> serde_json::Value {
        serde_json::from_str(
            &s.asset(Parameters(AssetRequest {
                action: "list".into(),
                id: None,
                item_id: Some(item_id.to_string()),
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn handoff_tool_requires_recipient_and_assigns_item() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = handoff_harness();

            // A blank recipient is rejected — the whole reason this tool exists.
            let err = s
                .handoff(Parameters(HandoffRequest {
                    recipient: "  ".into(),
                    name: "orphan".into(),
                    content: "for someone".into(),
                    ..Default::default()
                }))
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

            // A real handoff creates an item assigned to the recipient and
            // attaches the content to it as an asset.
            let result: serde_json::Value = serde_json::from_str(
                &s.handoff(Parameters(HandoffRequest {
                    recipient: "opencode".into(),
                    name: "review-packet".into(),
                    content: "please review".into(),
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap();
            let item_id = result["item_id"].as_str().unwrap().to_string();
            assert_eq!(result["recipient"], "opencode");
            assert_eq!(result["asset_version"], 1);

            let item: serde_json::Value = serde_json::from_str(
                &s.item(Parameters(ItemRequest {
                    action: "get".into(),
                    id: Some(item_id.clone()),
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(item["name"], "review-packet");
            assert_eq!(item["assignee_agent"], "opencode");

            let assets = item_assets(&s, &item_id);
            assert_eq!(assets.as_array().unwrap().len(), 1);
            assert_eq!(assets[0]["filename"], format!("{item_id}.md"));
        });
    }

    #[test]
    fn handoff_trims_whitespace_padded_recipient() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = handoff_harness();

            // A whitespace-padded recipient passes the emptiness check but must
            // still be stored trimmed, or exact-match assignee lookups miss it.
            let result: serde_json::Value = serde_json::from_str(
                &s.handoff(Parameters(HandoffRequest {
                    recipient: "  opencode  ".into(),
                    name: "review-packet".into(),
                    content: "please review".into(),
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(result["recipient"], "opencode");

            let item_id = result["item_id"].as_str().unwrap().to_string();
            let item: serde_json::Value = serde_json::from_str(
                &s.item(Parameters(ItemRequest {
                    action: "get".into(),
                    id: Some(item_id),
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(item["assignee_agent"], "opencode");
        });
    }

    #[test]
    fn handoff_with_item_id_assigns_existing_item_and_versions_the_asset() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = handoff_harness();
            let created: serde_json::Value = serde_json::from_str(
                &s.item(Parameters(empty_item_create("Existing task")))
                    .unwrap(),
            )
            .unwrap();
            let item_id = created["id"].as_str().unwrap().to_string();

            let first: serde_json::Value = serde_json::from_str(
                &s.handoff(Parameters(HandoffRequest {
                    recipient: "opencode".into(),
                    name: "Existing task".into(),
                    content: "v1 content".into(),
                    item_id: Some(item_id.clone()),
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(first["item_id"], item_id);
            assert_eq!(first["asset_version"], 1);

            // A different brief/name on the reply must not reset the version
            // chain — it's keyed on item_id, not name.
            let second: serde_json::Value = serde_json::from_str(
                &s.handoff(Parameters(HandoffRequest {
                    recipient: "opencode".into(),
                    name: "Addressed feedback".into(),
                    content: "v2 content".into(),
                    item_id: Some(item_id.clone()),
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(second["asset_version"], 2);

            // no duplicate item was created
            let item: serde_json::Value = serde_json::from_str(
                &s.item(Parameters(ItemRequest {
                    action: "get".into(),
                    id: Some(item_id.clone()),
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(item["assignee_agent"], "opencode");
            assert_eq!(item_assets(&s, &item_id).as_array().unwrap().len(), 2);
        });
    }

    #[test]
    fn artifact_diff_tool_returns_unified_diff() {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            artifacts_dir_override: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let first: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "publish".into(),
                name: Some("doc".into()),
                content: Some("alpha\nbeta\n".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let id = first["id"].as_str().unwrap().to_string();
        s.artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("doc".into()),
            content: Some("alpha\ngamma\n".into()),
            update_id: Some(id.clone()),
            ..Default::default()
        }))
        .unwrap();

        // to_version omitted = latest
        let diff = s
            .artifact(Parameters(ArtifactRequest {
                action: "diff".into(),
                id: Some(id),
                from_version: Some(1),
                to_version: None,
                ..Default::default()
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
        s.artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("alpha".into()),
            content: Some("there is a hidden NEEDLE in here".into()),
            ..Default::default()
        }))
        .unwrap();
        s.artifact(Parameters(ArtifactRequest {
            action: "publish".into(),
            name: Some("beta".into()),
            content: Some("nothing to see".into()),
            ..Default::default()
        }))
        .unwrap();

        let hits: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "search".into(),
                query: Some("needle".into()),
                session_id: None,
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(hits.as_array().unwrap().len(), 1);
        assert_eq!(hits[0]["name"], "alpha");
        assert!(
            hits[0]["snippet"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("needle"),
            "{hits}"
        );

        let by_name: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "search".into(),
                query: Some("beta".into()),
                session_id: None,
                ..Default::default()
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
            &s.artifact(Parameters(ArtifactRequest {
                action: "publish".into(),
                name: Some("prov".into()),
                content: Some("x".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let got: serde_json::Value = serde_json::from_str(
            &s.artifact(Parameters(ArtifactRequest {
                action: "get".into(),
                id: Some(out["id"].as_str().unwrap().into()),
                version: None,
                ..Default::default()
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
        let sender_of = |req: ArtifactRequest| -> serde_json::Value {
            let out: serde_json::Value =
                serde_json::from_str(&s.artifact(Parameters(req)).unwrap()).unwrap();
            let got: serde_json::Value = serde_json::from_str(
                &s.artifact(Parameters(ArtifactRequest {
                    action: "get".into(),
                    id: Some(out["id"].as_str().unwrap().into()),
                    version: None,
                    ..Default::default()
                }))
                .unwrap(),
            )
            .unwrap();
            got["sender"].clone()
        };

        let defaulted = sender_of(ArtifactRequest {
            action: "publish".into(),
            name: Some("defaulted".into()),
            content: Some("x".into()),
            ..Default::default()
        });
        assert_eq!(defaulted, "opencode");

        // ArtifactRequest has no `sender` field (removed in #75): authorship is
        // always the server-derived identity, so a caller cannot attribute a
        // published artifact to another agent. The spoof is unrepresentable at
        // the type level — stronger than a runtime "override ignored" check.
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
        assert_eq!(
            AgentflareMcp::identity(Some(String::new())),
            agent_detector::agent_name()
        );
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
                .artifact(Parameters(ArtifactRequest {
                    action: "publish".into(),
                    name: Some(name.into()),
                    r#type: None,
                    content: Some(content.into()),
                    session_id: None,
                    update_id: None,
                    ..Default::default()
                }))
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        }
    }

    /// Guards against the exact bug Phase 2's spec was written to avoid: a
    /// second, untagged `impl AgentflareMcp` block would compile fine and
    /// its `#[tool]` methods would still be directly callable (which is why
    /// unit tests calling them would pass either way) but be invisible to
    /// every real MCP client. Not fully sufficient on its own (see the
    /// spec) but catches the single-router invariant cheaply.
    #[test]
    fn exactly_one_tool_router_block_exists() {
        // Matches the attribute directly annotating `impl AgentflareMcp {`,
        // not every prose mention of it (e.g. the placement-rule doc comment
        // on the memory tools, or this test's own description).
        let marker = ["#[", "tool_router", "]\nimpl AgentflareMcp {"].concat();
        let src = include_str!("mcp_server.rs");
        assert_eq!(
            src.matches(&marker).count(),
            1,
            "all #[tool] methods must live in the one tool-router-tagged impl block"
        );
    }

    fn harness() -> (tempfile::TempDir, AgentflareMcp) {
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            backend_db_override: Some(tmp.path().join("backend.db")),
            backend_project_link_override: Some(tmp.path().join("project.json")),
            ..Default::default()
        };
        (tmp, s)
    }

    fn backend_conn(tmp: &tempfile::TempDir) -> rusqlite::Connection {
        agentflare_backend::db::open_db(&tmp.path().join("backend.db")).unwrap()
    }

    fn empty_item_create(name: &str) -> ItemRequest {
        ItemRequest {
            action: "create".into(),
            name: Some(name.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn item_create_auto_provisions_workspace_and_project() {
        let (_tmp, s) = harness();
        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test Item"))).unwrap())
                .unwrap();
        assert_eq!(created["name"], "Test Item");
        assert_eq!(created["sequence_id"], 1);
        assert!(created["project_id"].as_str().is_some());
    }

    #[test]
    fn item_create_rejects_empty_name() {
        let (_tmp, s) = harness();
        let err = s.item(Parameters(empty_item_create(""))).unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn item_update_state_sets_timestamps_via_mcp() {
        let (tmp, s) = harness();
        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();
        let project_id = created["project_id"].as_str().unwrap().to_string();

        let started_state_id = {
            let conn = backend_conn(&tmp);
            agentflare_backend::state::list_by_project(&conn, &project_id)
                .unwrap()
                .into_iter()
                .find(|st| st.group_name == "started")
                .unwrap()
                .id
        };

        let updated: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "update_state".into(),
                id: Some(item_id),
                state_id: Some(started_state_id),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(updated["started_at"].is_number());
        assert!(updated["completed_at"].is_null());
    }

    #[test]
    fn item_cancel_moves_to_cancelled_state() {
        let (tmp, s) = harness();
        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();
        let project_id = created["project_id"].as_str().unwrap().to_string();

        let cancelled: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "cancel".into(),
                id: Some(item_id),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let state_id = cancelled["state_id"].as_str().unwrap().to_string();

        let conn = backend_conn(&tmp);
        let group = agentflare_backend::state::list_by_project(&conn, &project_id)
            .unwrap()
            .into_iter()
            .find(|st| st.id == state_id)
            .unwrap()
            .group_name;
        assert_eq!(group, "cancelled");
    }

    #[test]
    fn item_cancel_releases_the_callers_own_claim() {
        // `claim` always resolves a worktree_repo_root and may run real `git
        // worktree` commands against it — every test that calls `claim` must
        // override this to an isolated throwaway repo, never the repo
        // `cargo test` itself is running in. Same scaffolding as
        // `item_claim_response_includes_worktree_path`.
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        let repo_root = repo_dir.path().to_path_buf();
        let run_git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo_root)
                .output()
                .unwrap()
        };
        run_git(&["init", "-b", "master"]);
        run_git(&["config", "user.email", "test@test.com"]);
        run_git(&["config", "user.name", "Test"]);
        run_git(&["commit", "--allow-empty", "-m", "initial"]);

        let s = AgentflareMcp {
            backend_db_override: Some(tmp.path().join("backend.db")),
            backend_project_link_override: Some(tmp.path().join("project.json")),
            worktree_repo_root_override: Some(repo_root),
            ..Default::default()
        };

        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap();

        s.item(Parameters(ItemRequest {
            action: "cancel".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap();

        // The claim must be released — re-claiming should succeed
        // immediately instead of coming back "held".
        let reclaimed: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "claim".into(),
                id: Some(item_id),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(reclaimed["status"], "acquired");
    }

    #[test]
    fn item_list_rejects_negative_limit_and_offset() {
        let (_tmp, s) = harness();
        let err = s
            .item(Parameters(ItemRequest {
                action: "list".into(),
                limit: Some(-1),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let err = s
            .item(Parameters(ItemRequest {
                action: "list".into(),
                offset: Some(-1),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn item_list_filters_by_assignee_or_unassigned_and_sorts_open_first() {
        let (tmp, s) = harness();
        let mine_open: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Mine open"))).unwrap())
                .unwrap();
        let project_id = mine_open["project_id"].as_str().unwrap().to_string();
        s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(mine_open["id"].as_str().unwrap().to_string()),
            assignee_agent: Some("me".into()),
            ..Default::default()
        }))
        .unwrap();

        serde_json::from_str::<serde_json::Value>(
            &s.item(Parameters(empty_item_create("Unassigned"))).unwrap(),
        )
        .unwrap();

        let others: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Others"))).unwrap())
                .unwrap();
        s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(others["id"].as_str().unwrap().to_string()),
            assignee_agent: Some("someone-else".into()),
            ..Default::default()
        }))
        .unwrap();

        let mine_done: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Mine done"))).unwrap())
                .unwrap();
        s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(mine_done["id"].as_str().unwrap().to_string()),
            assignee_agent: Some("me".into()),
            ..Default::default()
        }))
        .unwrap();
        let done_state_id = {
            let conn = backend_conn(&tmp);
            agentflare_backend::state::list_by_project(&conn, &project_id)
                .unwrap()
                .into_iter()
                .find(|st| st.group_name == "completed")
                .unwrap()
                .id
        };
        s.item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(mine_done["id"].as_str().unwrap().to_string()),
            state_id: Some(done_state_id),
            ..Default::default()
        }))
        .unwrap();

        let listed: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "list".into(),
                assignee_agent: Some("me".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let names: Vec<&str> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["Mine open", "Unassigned", "Mine done"]);
    }

    #[test]
    fn item_list_defaults_assignee_filter_to_server_identity() {
        // #75: a bare `item(list)` (no assignee_agent) must default to the
        // server-derived identity — mine + unassigned — not dump every item.
        let tmp = tempfile::tempdir().unwrap();
        let s = AgentflareMcp {
            backend_db_override: Some(tmp.path().join("backend.db")),
            backend_project_link_override: Some(tmp.path().join("project.json")),
            agent: Some("me".into()),
            ..Default::default()
        };

        let mine: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Mine"))).unwrap()).unwrap();
        s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(mine["id"].as_str().unwrap().to_string()),
            assignee_agent: Some("me".into()),
            ..Default::default()
        }))
        .unwrap();

        serde_json::from_str::<serde_json::Value>(
            &s.item(Parameters(empty_item_create("Unassigned"))).unwrap(),
        )
        .unwrap();

        let others: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Others"))).unwrap())
                .unwrap();
        s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(others["id"].as_str().unwrap().to_string()),
            assignee_agent: Some("someone-else".into()),
            ..Default::default()
        }))
        .unwrap();

        // Bare list: no assignee_agent → defaults to "me" (mine + unassigned).
        let defaulted: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "list".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let mut names: Vec<&str> = defaulted
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        names.sort_unstable();
        assert_eq!(names, vec!["Mine", "Unassigned"]);

        // An explicit assignee_agent is still honored (view a teammate's queue).
        let explicit: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "list".into(),
                assignee_agent: Some("someone-else".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let mut names2: Vec<&str> = explicit
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        names2.sort_unstable();
        assert_eq!(names2, vec!["Others", "Unassigned"]);
    }

    #[test]
    fn item_list_state_group_filter_accepts_comma_separated_groups() {
        let (tmp, s) = harness();
        let open_item: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Open"))).unwrap()).unwrap();
        let project_id = open_item["project_id"].as_str().unwrap().to_string();
        let done_item: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Done"))).unwrap()).unwrap();
        let cancelled_item: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Cancelled"))).unwrap())
                .unwrap();

        let conn = backend_conn(&tmp);
        let states = agentflare_backend::state::list_by_project(&conn, &project_id).unwrap();
        let done_state_id = states
            .iter()
            .find(|st| st.group_name == "completed")
            .unwrap()
            .id
            .clone();
        let cancelled_state_id = states
            .iter()
            .find(|st| st.group_name == "cancelled")
            .unwrap()
            .id
            .clone();
        drop(conn);

        s.item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(done_item["id"].as_str().unwrap().to_string()),
            state_id: Some(done_state_id),
            ..Default::default()
        }))
        .unwrap();
        s.item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(cancelled_item["id"].as_str().unwrap().to_string()),
            state_id: Some(cancelled_state_id),
            ..Default::default()
        }))
        .unwrap();

        let listed: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "list".into(),
                state_group: Some("backlog,completed".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let names: Vec<&str> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["Open", "Done"]);
    }

    #[test]
    fn item_list_respects_limit_and_offset() {
        let (_tmp, s) = harness();
        for name in ["A", "B", "C"] {
            s.item(Parameters(empty_item_create(name))).unwrap();
        }
        let listed: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "list".into(),
                limit: Some(1),
                offset: Some(1),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let names: Vec<&str> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["B"]);
    }

    #[test]
    fn item_list_returns_lean_projection_with_readable_state() {
        let (_tmp, s) = harness();
        s.item(Parameters(empty_item_create("Test"))).unwrap();
        let listed: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "list".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let first = &listed.as_array().unwrap()[0];
        assert_eq!(first["state"], "Backlog");
        assert_eq!(first["state_group"], "backlog");
        assert!(first.get("description").is_none());
        assert!(first.get("metadata").is_none());
    }

    #[test]
    fn resolve_workspace_id_creates_once_and_reuses() {
        let (tmp, _s) = harness();
        let conn = backend_conn(&tmp);
        let id1 = AgentflareMcp::resolve_workspace_id(&conn).unwrap();
        let id2 = AgentflareMcp::resolve_workspace_id(&conn).unwrap();
        assert_eq!(id1, id2);
    }

    /// If `.agentflare/project.json` is deleted (wiped worktree, `rm -rf`,
    /// etc.) while the project it pointed to still exists, resolving again
    /// must reconnect to that same project — not silently fork a duplicate,
    /// which would strand the original project's items.
    #[test]
    fn resolve_project_relinks_to_existing_project_when_link_file_is_deleted() {
        let (tmp, s) = harness();
        let conn = backend_conn(&tmp);
        let first = s.resolve_project(&conn).unwrap();

        std::fs::remove_file(s.project_link_path()).unwrap();

        let second = s.resolve_project(&conn).unwrap();
        assert_eq!(
            first.id, second.id,
            "must reconnect to the same project, not fork a duplicate"
        );
        let all =
            agentflare_backend::project::list_by_workspace(&conn, &first.workspace_id).unwrap();
        assert_eq!(
            all.len(),
            1,
            "no duplicate project should have been created: {all:?}"
        );
    }

    /// Two different repos can easily share a directory basename (or, for
    /// non-git dirs, no distinguishing info at all beyond the name). They
    /// must never be conflated into one project just because they'd derive
    /// the same display identifier — each gets its own project, with the
    /// second disambiguated by a suffix.
    #[test]
    fn resolve_project_does_not_conflate_different_repos_with_the_same_derived_name() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("backend.db");
        let s1 = AgentflareMcp {
            backend_db_override: Some(db_path.clone()),
            backend_project_link_override: Some(tmp.path().join("link1.json")),
            backend_repo_key_override: Some("path:/repo/one".to_string()),
            ..Default::default()
        };
        let s2 = AgentflareMcp {
            backend_db_override: Some(db_path.clone()),
            backend_project_link_override: Some(tmp.path().join("link2.json")),
            backend_repo_key_override: Some("path:/repo/two".to_string()),
            ..Default::default()
        };
        let conn = agentflare_backend::db::open_db(&db_path).unwrap();
        let p1 = s1.resolve_project(&conn).unwrap();
        let p2 = s2.resolve_project(&conn).unwrap();
        assert_ne!(
            p1.id, p2.id,
            "different repos must never share a project even with the same derived name"
        );
        assert_ne!(
            p1.identifier, p2.identifier,
            "the second project must get a disambiguating suffix"
        );

        // Each keeps resolving to its own project on repeat calls.
        assert_eq!(s1.resolve_project(&conn).unwrap().id, p1.id);
        assert_eq!(s2.resolve_project(&conn).unwrap().id, p2.id);
    }

    /// Non-git projects need the same "root is stable no matter which
    /// subdirectory you're in" guarantee git repos get for free from `git
    /// rev-parse --show-toplevel` — otherwise the same project would split
    /// across multiple `.agentflare/project.json` files depending on which
    /// subdirectory a tool happened to be called from.
    #[test]
    fn find_root_from_walks_up_to_the_nearest_marker() {
        // Bounding "home" at the tempdir's own parent contains the walk
        // entirely within this test's constructed tree — passing some
        // unrelated path here would NOT do that: the walk follows the real
        // filesystem's `.parent()` chain regardless, so it would keep
        // climbing past `root` into real ancestor directories (which may
        // have their own real markers, e.g. this machine's actual
        // `~/.agentflare`) until it happened to reach that unrelated path,
        // which — not being a real ancestor — it never would, walking all
        // the way to the filesystem root instead.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.parent().unwrap();
        std::fs::write(root.join("package.json"), "{}").unwrap();
        let deep = root.join("src").join("nested").join("deep");
        std::fs::create_dir_all(&deep).unwrap();

        assert_eq!(AgentflareMcp::find_root_from(&deep, home), root);
        assert_eq!(AgentflareMcp::find_root_from(root, home), root);
    }

    #[test]
    fn find_root_from_prefers_an_existing_agentflare_link_over_other_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.parent().unwrap();
        // A nested directory with its own marker (e.g. a sub-package) must
        // not shadow an ancestor's existing project link — the
        // .agentflare pass runs before the ROOT_MARKERS pass for
        // exactly this reason.
        std::fs::create_dir_all(root.join(".agentflare")).unwrap();
        let sub = root.join("packages").join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("package.json"), "{}").unwrap();

        assert_eq!(AgentflareMcp::find_root_from(&sub, home), root);
        assert_eq!(AgentflareMcp::find_root_from(root, home), root);
    }

    /// The boundary itself: a directory that IS `home` must never be
    /// treated as a project root, even if it happens to contain a marker —
    /// this is what keeps the global `~/.agentflare` data dir from ever
    /// being mistaken for a per-repo link.
    #[test]
    fn find_root_from_never_resolves_to_home_itself() {
        let home = tempfile::tempdir().unwrap();
        // Stands in for the real global data dir at ~/.agentflare.
        std::fs::create_dir_all(home.path().join(".agentflare")).unwrap();
        let start = home.path().join("some_project");
        std::fs::create_dir_all(&start).unwrap();

        // `start` itself has no marker, and home — one level up — does. If
        // the walk checked markers at `home`, this would return `home`. It
        // must instead stop short of ever inspecting `home` and fall back
        // to `start`.
        assert_eq!(AgentflareMcp::find_root_from(&start, home.path()), start);
    }

    // No test for the "nothing found anywhere above" fallback: `find_root_from`
    // walks all the way to the filesystem root, so a tempdir-based test would
    // depend on what markers happen to exist above the OS temp directory on
    // whatever machine runs this — not a property this test can control. The
    // fallback itself is a single trivial `None => return start`.

    #[test]
    fn asset_attach_get_list_delete_round_trip() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let home = crate::paths::home();
            let staging = home.join(".agentflare").join("staging");
            std::fs::create_dir_all(&staging).unwrap();

            let item: serde_json::Value =
                serde_json::from_str(&s.item(Parameters(empty_item_create("asset-test"))).unwrap())
                    .unwrap();
            let item_id = item["id"].as_str().unwrap().to_string();

            let content = b"hello asset test";
            std::fs::write(staging.join("test.txt"), content).unwrap();

            let attached: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some(item_id.clone()),
                    project_id: None,
                    filename: Some("test.txt".into()),
                    metadata: Some(r#"{"source":"test"}"#.into()),
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(attached["filename"], "test.txt");
            let asset_id = attached["id"].as_str().unwrap().to_string();

            let got: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "get".into(),
                    id: Some(asset_id.clone()),
                    item_id: None,
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(got["asset"]["filename"], "test.txt");
            assert!(got["content"].as_str().is_some());

            let list: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "list".into(),
                    id: None,
                    item_id: Some(item_id.clone()),
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(list.as_array().unwrap().len(), 1);
            assert_eq!(list[0]["id"], asset_id);

            let del: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "delete".into(),
                    id: Some(asset_id.clone()),
                    item_id: None,
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(del["deleted"], true);

            let after: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "list".into(),
                    id: None,
                    item_id: Some(item_id),
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            assert!(after.as_array().unwrap().is_empty());
        });
    }

    #[test]
    fn asset_attach_rejects_path_traversal() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let err = s
                .asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some("item-1".into()),
                    project_id: None,
                    filename: Some("../etc/hosts".into()),
                    metadata: None,
                }))
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        });
    }

    #[test]
    fn asset_attach_rejects_missing_filename() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let err = s
                .asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some("item-1".into()),
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        });
    }

    #[test]
    fn asset_attach_rejects_both_item_and_project() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let err = s
                .asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some("item-1".into()),
                    project_id: Some("proj-1".into()),
                    filename: Some("anything.txt".into()),
                    metadata: None,
                }))
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        });
    }

    #[test]
    fn asset_get_rejects_missing_id() {
        let (_tmp, s) = harness();
        let err = s
            .asset(Parameters(AssetRequest {
                action: "get".into(),
                id: None,
                item_id: None,
                project_id: None,
                filename: None,
                metadata: None,
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn asset_shared_storage_delete_safety() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let home = crate::paths::home();
            let staging = home.join(".agentflare").join("staging");
            std::fs::create_dir_all(&staging).unwrap();

            let item1: serde_json::Value =
                serde_json::from_str(&s.item(Parameters(empty_item_create("shared-1"))).unwrap())
                    .unwrap();
            let item2: serde_json::Value =
                serde_json::from_str(&s.item(Parameters(empty_item_create("shared-2"))).unwrap())
                    .unwrap();
            let id1 = item1["id"].as_str().unwrap().to_string();
            let id2 = item2["id"].as_str().unwrap().to_string();

            let content = b"same content for shared delete test";
            std::fs::write(staging.join("shared.txt"), content).unwrap();
            let asset1: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some(id1.clone()),
                    project_id: None,
                    filename: Some("shared.txt".into()),
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            let a1_id = asset1["id"].as_str().unwrap().to_string();

            // re-stage the same content for item2
            std::fs::write(staging.join("shared.txt"), content).unwrap();
            let asset2: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some(id2.clone()),
                    project_id: None,
                    filename: Some("shared.txt".into()),
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            let a2_id = asset2["id"].as_str().unwrap().to_string();

            // delete first — second should still be readable
            let del1: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "delete".into(),
                    id: Some(a1_id.clone()),
                    item_id: None,
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(del1["deleted"], true);

            let got2_raw = s
                .asset(Parameters(AssetRequest {
                    action: "get".into(),
                    id: Some(a2_id.clone()),
                    item_id: None,
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap();
            let got2: serde_json::Value = serde_json::from_str(&got2_raw).unwrap();
            assert!(
                got2["content"].as_str().is_some(),
                "item2 must still be readable after item1 deletion: {got2_raw}"
            );

            // delete second — now file should be gone
            let del2: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "delete".into(),
                    id: Some(a2_id.clone()),
                    item_id: None,
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(del2["deleted"], true);
        });
    }

    #[test]
    fn asset_content_dedup() {
        crate::paths::test_support::with_temp_home(|| {
            let (tmp, s) = harness();
            let home = crate::paths::home();
            let staging = home.join(".agentflare").join("staging");
            std::fs::create_dir_all(&staging).unwrap();

            let item_a: serde_json::Value =
                serde_json::from_str(&s.item(Parameters(empty_item_create("dedup-a"))).unwrap())
                    .unwrap();
            let item_b: serde_json::Value =
                serde_json::from_str(&s.item(Parameters(empty_item_create("dedup-b"))).unwrap())
                    .unwrap();
            let id_a = item_a["id"].as_str().unwrap().to_string();
            let id_b = item_b["id"].as_str().unwrap().to_string();

            let content = b"dedup me please";
            std::fs::write(staging.join("dedup.txt"), content).unwrap();
            s.asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some(id_a.clone()),
                project_id: None,
                filename: Some("dedup.txt".into()),
                metadata: None,
            }))
            .unwrap();

            std::fs::write(staging.join("dedup.txt"), content).unwrap();
            s.asset(Parameters(AssetRequest {
                action: "attach".into(),
                id: None,
                item_id: Some(id_b.clone()),
                project_id: None,
                filename: Some("dedup.txt".into()),
                metadata: None,
            }))
            .unwrap();

            // two rows, one file on disk: count unique storage_path values
            let conn = backend_conn(&tmp);
            let unique_paths: i64 = conn
                .query_row(
                    "SELECT count(DISTINCT storage_path) FROM assets WHERE deleted_at IS NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(unique_paths, 1, "same content must share one storage_path");

            let total_rows: i64 = conn
                .query_row(
                    "SELECT count(*) FROM assets WHERE deleted_at IS NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(total_rows, 2, "two rows despite one file on disk");
        });
    }

    #[test]
    fn asset_attach_to_project() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let home = crate::paths::home();
            let staging = home.join(".agentflare").join("staging");
            std::fs::create_dir_all(&staging).unwrap();

            let project: serde_json::Value = serde_json::from_str(
                &s.project(Parameters(ProjectRequest {
                    action: "info".into(),
                }))
                .unwrap(),
            )
            .unwrap();
            let project_id = project["id"].as_str().unwrap().to_string();

            std::fs::write(staging.join("project-file.txt"), b"project attachment").unwrap();
            let attached: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: None,
                    project_id: Some(project_id.clone()),
                    filename: Some("project-file.txt".into()),
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(attached["filename"], "project-file.txt");

            let list: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "list".into(),
                    id: None,
                    item_id: None,
                    project_id: Some(project_id),
                    filename: None,
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(list.as_array().unwrap().len(), 1);
        });
    }

    #[test]
    fn asset_attach_rejects_neither_item_nor_project() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let err = s
                .asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: None,
                    project_id: None,
                    filename: Some("anything.txt".into()),
                    metadata: None,
                }))
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        });
    }

    #[test]
    fn asset_attach_rejects_nonexistent_item() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let home = crate::paths::home();
            let staging = home.join(".agentflare").join("staging");
            std::fs::create_dir_all(&staging).unwrap();
            std::fs::write(staging.join("f.txt"), b"data").unwrap();
            let err = s
                .asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some("nonexistent-item".into()),
                    project_id: None,
                    filename: Some("f.txt".into()),
                    metadata: None,
                }))
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        });
    }

    #[test]
    fn asset_attach_rejects_missing_staging_file() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let err = s
                .asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some("item-1".into()),
                    project_id: None,
                    filename: Some("does-not-exist.txt".into()),
                    metadata: None,
                }))
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        });
    }

    #[test]
    fn asset_attach_rejects_oversized_file() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let home = crate::paths::home();
            let staging = home.join(".agentflare").join("staging");
            std::fs::create_dir_all(&staging).unwrap();
            // write a file just past the default 5 MB limit
            let big = vec![0u8; 5 * 1024 * 1024 + 1];
            std::fs::write(staging.join("big.bin"), &big).unwrap();
            let err = s
                .asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some("item-1".into()),
                    project_id: None,
                    filename: Some("big.bin".into()),
                    metadata: None,
                }))
                .unwrap_err();
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        });
    }

    #[test]
    fn asset_get_over_max_inline_omits_content() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let home = crate::paths::home();
            let staging = home.join(".agentflare").join("staging");
            std::fs::create_dir_all(&staging).unwrap();

            let item: serde_json::Value = serde_json::from_str(
                &s.item(Parameters(empty_item_create("big-inline-test")))
                    .unwrap(),
            )
            .unwrap();
            let item_id = item["id"].as_str().unwrap().to_string();

            // write a small file, but set a tiny inline cap for this test
            std::fs::write(staging.join("small.txt"), b"hello inline cap").unwrap();
            let attached: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some(item_id.clone()),
                    project_id: None,
                    filename: Some("small.txt".into()),
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            let asset_id = attached["id"].as_str().unwrap().to_string();

            // override inline limit to 1 byte so our file exceeds it
            // SAFETY: with_temp_home holds GLOBAL_STATE_LOCK so no concurrent env mutation.
            let saved = std::env::var("AGENTFLARE_BACKEND_ASSET_MAX_INLINE_BYTES").ok();
            unsafe { std::env::set_var("AGENTFLARE_BACKEND_ASSET_MAX_INLINE_BYTES", "1") };
            let got: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "get".into(),
                    id: Some(asset_id),
                    item_id: None,
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            assert!(got["content"].is_null());
            assert!(got["content_omitted_reason"].as_str().is_some());
            // restore to avoid leaking to sibling tests
            match saved {
                Some(v) => unsafe {
                    std::env::set_var("AGENTFLARE_BACKEND_ASSET_MAX_INLINE_BYTES", v)
                },
                None => unsafe {
                    std::env::remove_var("AGENTFLARE_BACKEND_ASSET_MAX_INLINE_BYTES")
                },
            }
        });
    }

    #[test]
    fn asset_get_returns_text_content_as_utf8_not_base64() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let home = crate::paths::home();
            let staging = home.join(".agentflare").join("staging");
            std::fs::create_dir_all(&staging).unwrap();

            let item: serde_json::Value = serde_json::from_str(
                &s.item(Parameters(empty_item_create("utf8-content-test")))
                    .unwrap(),
            )
            .unwrap();
            let item_id = item["id"].as_str().unwrap().to_string();

            let body = "# Handoff\n\nImplement the fix \u{2192} land a PR. \u{2713}";
            std::fs::write(staging.join("note.md"), body.as_bytes()).unwrap();
            let attached: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some(item_id.clone()),
                    project_id: None,
                    filename: Some("note.md".into()),
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            let asset_id = attached["id"].as_str().unwrap().to_string();

            let got: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "get".into(),
                    id: Some(asset_id),
                    item_id: None,
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();

            // Text assets must come back as readable UTF-8, not base64.
            assert_eq!(got["encoding"].as_str(), Some("utf8"));
            assert_eq!(got["content"].as_str(), Some(body));
        });
    }

    #[test]
    fn asset_get_returns_base64_for_binary_with_valid_utf8_bytes() {
        crate::paths::test_support::with_temp_home(|| {
            let (_tmp, s) = harness();
            let home = crate::paths::home();
            let staging = home.join(".agentflare").join("staging");
            std::fs::create_dir_all(&staging).unwrap();

            let item: serde_json::Value = serde_json::from_str(
                &s.item(Parameters(empty_item_create("binary-utf8-test")))
                    .unwrap(),
            )
            .unwrap();
            let item_id = item["id"].as_str().unwrap().to_string();

            // Bytes 0x00,0x01,0x02,0x03 are valid UTF-8, but this is an
            // octet-stream (.bin) asset: it must come back Base64, not "utf8".
            let raw = [0u8, 1, 2, 3];
            assert!(
                std::str::from_utf8(&raw).is_ok(),
                "precondition: valid UTF-8"
            );
            std::fs::write(staging.join("blob.bin"), raw).unwrap();
            let attached: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "attach".into(),
                    id: None,
                    item_id: Some(item_id.clone()),
                    project_id: None,
                    filename: Some("blob.bin".into()),
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();
            let asset_id = attached["id"].as_str().unwrap().to_string();

            let got: serde_json::Value = serde_json::from_str(
                &s.asset(Parameters(AssetRequest {
                    action: "get".into(),
                    id: Some(asset_id),
                    item_id: None,
                    project_id: None,
                    filename: None,
                    metadata: None,
                }))
                .unwrap(),
            )
            .unwrap();

            // Binary MIME => Base64 regardless of UTF-8 validity.
            assert_eq!(got["encoding"].as_str(), Some("base64"));
        });
    }

    #[test]
    fn item_comment_create_and_list_roundtrip() {
        let (_tmp, s) = harness();
        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        let comment: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some("Hello, world!".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(comment["body"], "Hello, world!");
        assert!(comment["author_agent"].as_str().unwrap().contains(':'));

        let comments: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "list".into(),
                item_id: Some(item_id),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let arr = comments.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["body"], "Hello, world!");
    }

    #[test]
    fn item_comment_rejects_empty_body() {
        let (_tmp, s) = harness();
        let err = s
            .comment(Parameters(CommentRequest {
                action: "create".into(),
                item_id: Some("item-1".into()),
                body: Some("".into()),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn item_comment_edit_succeeds_when_latest_and_own_and_unclaimed_by_other() {
        let (_tmp, s) = harness();
        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        let comment: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some("original".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let comment_id = comment["id"].as_str().unwrap().to_string();

        let updated: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "edit".into(),
                id: Some(comment_id.clone()),
                body: Some("edited".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(updated["body"], "edited");
    }

    #[test]
    fn item_comment_edit_rejected_when_comment_not_found() {
        let (_tmp, s) = harness();
        let err = s
            .comment(Parameters(CommentRequest {
                action: "edit".into(),
                id: Some("nonexistent".into()),
                body: Some("edited".into()),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn item_comment_edit_rejected_when_different_agent() {
        let (_tmp, s) = harness();
        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        let comment_id = s
            .with_backend_db(|conn| {
                agentflare_backend::comment::create(conn, &item_id, "someone-else:1", "not mine")
                    .unwrap()
                    .id
            })
            .unwrap();

        let err = s
            .comment(Parameters(CommentRequest {
                action: "edit".into(),
                id: Some(comment_id),
                body: Some("edited".into()),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("own comments"));
    }

    #[test]
    fn item_comment_edit_succeeds_across_sessions_of_same_agent() {
        let (_tmp, s) = harness();
        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        // Same agent, different session instance — e.g. a prior CLI
        // invocation, or an MCP server process that has since restarted.
        let agent = crate::claims::agent_of(&crate::claims::owner_id()).to_string();
        let earlier_session_author = format!("{agent}:some-earlier-session");

        let comment_id = s
            .with_backend_db(|conn| {
                agentflare_backend::comment::create(
                    conn,
                    &item_id,
                    &earlier_session_author,
                    "mine, from an earlier session",
                )
                .unwrap()
                .id
            })
            .unwrap();

        let updated: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "edit".into(),
                id: Some(comment_id),
                body: Some("edited".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(updated["body"], "edited");
    }

    #[test]
    fn item_comment_edit_uses_id_tiebreak_when_timestamps_collide() {
        let (_tmp, s) = harness();
        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        let first: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some("first".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let first_id = first["id"].as_str().unwrap().to_string();

        let second: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id),
                body: Some("second".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let second_id = second["id"].as_str().unwrap().to_string();

        // Force both comments onto the same second-resolution timestamp, as
        // happens routinely under real multi-agent traffic. Only the comment
        // with the higher (later) UUIDv7 id should still count as latest.
        s.with_backend_db(|conn| {
            conn.execute(
                "UPDATE item_comments SET created_at = 1000, updated_at = 1000",
                [],
            )
            .unwrap();
        })
        .unwrap();

        let err = s
            .comment(Parameters(CommentRequest {
                action: "edit".into(),
                id: Some(first_id),
                body: Some("edited".into()),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let updated: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "edit".into(),
                id: Some(second_id),
                body: Some("edited".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(updated["body"], "edited");
    }

    #[test]
    fn item_comment_delete_succeeds_when_latest_and_own() {
        let (_tmp, s) = harness();
        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        let comment: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id),
                body: Some("delete-me".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let comment_id = comment["id"].as_str().unwrap().to_string();

        let result: serde_json::Value = serde_json::from_str(
            &s.comment(Parameters(CommentRequest {
                action: "delete".into(),
                id: Some(comment_id),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["deleted"], true);
    }

    #[test]
    fn item_claim_response_includes_worktree_path() {
        let tmp = tempfile::tempdir().unwrap();
        // Isolated temp repo — this test must never run real `git
        // worktree`/branch operations against the actual repository running
        // the test suite.
        let repo_dir = tempfile::tempdir().unwrap();
        let repo_root = repo_dir.path().to_path_buf();
        let run_git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo_root)
                .output()
                .unwrap()
        };
        run_git(&["init", "-b", "master"]);
        run_git(&["config", "user.email", "test@test.com"]);
        run_git(&["config", "user.name", "Test"]);
        run_git(&["commit", "--allow-empty", "-m", "initial"]);

        let s = AgentflareMcp {
            backend_db_override: Some(tmp.path().join("backend.db")),
            backend_project_link_override: Some(tmp.path().join("project.json")),
            worktree_repo_root_override: Some(repo_root),
            ..Default::default()
        };

        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        let result: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "claim".into(),
                id: Some(item_id),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["status"], "acquired");
        assert!(result.get("worktree_path").is_some());
        let path = result["worktree_path"].as_str().unwrap();
        assert!(std::path::Path::new(path).exists());
        // `next` is now a protocol-level decoration injected by
        // `call_tool`, not in the direct method output.
        assert!(result.get("next").is_none());
    }

    #[test]
    fn item_done_without_new_commits_omits_pr_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_dir = tempfile::tempdir().unwrap();
        let repo_root = repo_dir.path().to_path_buf();
        let run_git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(&repo_root)
                .output()
                .unwrap()
        };
        run_git(&["init", "-b", "master"]);
        run_git(&["config", "user.email", "test@test.com"]);
        run_git(&["config", "user.name", "Test"]);
        run_git(&["commit", "--allow-empty", "-m", "initial"]);

        let s = AgentflareMcp {
            backend_db_override: Some(tmp.path().join("backend.db")),
            backend_project_link_override: Some(tmp.path().join("project.json")),
            worktree_repo_root_override: Some(repo_root),
            ..Default::default()
        };

        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap();

        // No commits were made in the claimed worktree, so `done` has
        // nothing to push/PR — must not attempt a real push (no remote
        // configured on this throwaway repo).
        let result: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "done".into(),
                id: Some(item_id),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(result["done"], true);
        assert!(result.get("pr_url").is_none());
        assert!(result.get("next").is_none());
    }

    #[test]
    fn item_rejects_unknown_action() {
        let (_tmp, s) = harness();
        let err = s
            .item(Parameters(ItemRequest {
                action: "nonexistent".into(),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn label_rejects_unknown_action() {
        let (_tmp, s) = harness();
        let err = s
            .label(Parameters(LabelRequest {
                action: "nonexistent".into(),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn label_create_list_update_delete_via_mcp() {
        let (_tmp, s) = harness();
        // create
        let created: serde_json::Value = serde_json::from_str(
            &s.label(Parameters(LabelRequest {
                action: "create".into(),
                name: Some("bug".into()),
                color: Some("#EF4444".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["name"], "bug");
        assert_eq!(created["color"], "#EF4444");

        // list shows it
        let listed: serde_json::Value = serde_json::from_str(
            &s.label(Parameters(LabelRequest {
                action: "list".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["id"], id);

        // update renames + recolors
        let updated: serde_json::Value = serde_json::from_str(
            &s.label(Parameters(LabelRequest {
                action: "update".into(),
                id: Some(id.clone()),
                name: Some("defect".into()),
                color: Some("#F59E0B".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(updated["name"], "defect");
        assert_eq!(updated["color"], "#F59E0B");

        // delete
        let deleted: serde_json::Value = serde_json::from_str(
            &s.label(Parameters(LabelRequest {
                action: "delete".into(),
                id: Some(id.clone()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(deleted["deleted"], true);

        // list is now empty
        let after: serde_json::Value = serde_json::from_str(
            &s.label(Parameters(LabelRequest {
                action: "list".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(after.as_array().unwrap().is_empty());
    }

    #[test]
    fn label_update_requires_id() {
        let (_tmp, s) = harness();
        let err = s
            .label(Parameters(LabelRequest {
                action: "update".into(),
                name: Some("x".into()),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn item_add_label_rejects_foreign_project_label_via_mcp() {
        let (tmp, s) = harness();
        // Auto-provisions this repo's workspace + project.
        let item: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
        let item_id = item["id"].as_str().unwrap().to_string();

        // A label belonging to a completely separate workspace/project.
        let foreign_label_id = {
            let conn = backend_conn(&tmp);
            let ws = agentflare_backend::workspace::create(
                &conn,
                agentflare_backend::workspace::CreateWorkspace {
                    name: "Other".into(),
                    slug: "other".into(),
                    owner_agent: None,
                    item_label: None,
                },
            )
            .unwrap();
            let proj = agentflare_backend::project::create(
                &conn,
                agentflare_backend::project::CreateProject {
                    workspace_id: ws.id.clone(),
                    name: "Other".into(),
                    identifier: "OTH".into(),
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap();
            agentflare_backend::label::create(
                &conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(proj.id),
                    workspace_id: ws.id,
                    name: "bug".into(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap()
            .id
        };

        let err = s
            .item(Parameters(ItemRequest {
                action: "add_label".into(),
                id: Some(item_id),
                label_id: Some(foreign_label_id),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn label_update_and_delete_reject_foreign_project_label() {
        let (tmp, s) = harness();

        // A label in a separate workspace/project, not the repo's resolved project.
        let foreign_label_id = {
            let conn = backend_conn(&tmp);
            let ws = agentflare_backend::workspace::create(
                &conn,
                agentflare_backend::workspace::CreateWorkspace {
                    name: "Other".into(),
                    slug: "other".into(),
                    owner_agent: None,
                    item_label: None,
                },
            )
            .unwrap();
            let proj = agentflare_backend::project::create(
                &conn,
                agentflare_backend::project::CreateProject {
                    workspace_id: ws.id.clone(),
                    name: "Other".into(),
                    identifier: "OTH".into(),
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap();
            agentflare_backend::label::create(
                &conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(proj.id),
                    workspace_id: ws.id,
                    name: "bug".into(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap()
            .id
        };

        let upd = s
            .label(Parameters(LabelRequest {
                action: "update".into(),
                id: Some(foreign_label_id.clone()),
                name: Some("hijacked".into()),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(upd.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let del = s
            .label(Parameters(LabelRequest {
                action: "delete".into(),
                id: Some(foreign_label_id.clone()),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(del.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        // The foreign label must survive both rejected attempts unchanged.
        let conn = backend_conn(&tmp);
        let survivor = agentflare_backend::label::get(&conn, &foreign_label_id).unwrap();
        assert_eq!(survivor.name, "bug");
    }

    #[test]
    fn webhook_rejects_unknown_action() {
        let (_tmp, s) = harness();
        let err = s
            .webhook(Parameters(WebhookRequest {
                action: "nonexistent".into(),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn project_rejects_unknown_action() {
        let (_tmp, s) = harness();
        let err = s
            .project(Parameters(ProjectRequest {
                action: "nonexistent".into(),
            }))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn next_hint_claim_with_worktree_path() {
        let json = serde_json::json!({"status": "acquired", "worktree_path": "/tmp/wt"});
        let hint = next_hint("item", &json).unwrap();
        assert!(hint.contains("worktree_path"), "{}", hint);
    }

    #[test]
    fn next_hint_done_with_pr_url() {
        let json = serde_json::json!({"done": true, "pr_url": "https://github.com/x/pull/1"});
        let hint = next_hint("item", &json).unwrap();
        assert!(hint.contains("review/merge"), "{}", hint);
    }

    #[test]
    fn next_hint_handoff_always_returns_hint() {
        let json = serde_json::json!({"item_id": "abc", "recipient": "x"});
        let hint = next_hint("handoff", &json).unwrap();
        assert!(hint.contains("inbox"), "{}", hint);
    }

    #[test]
    fn next_hint_unknown_tool_returns_none() {
        let json = serde_json::json!({"result": "ok"});
        assert!(next_hint("asset", &json).is_none());
    }

    #[test]
    fn next_hint_item_without_trigger_fields_returns_none() {
        let json = serde_json::json!({"done": true});
        assert!(next_hint("item", &json).is_none());
    }

    #[test]
    fn next_hint_non_object_input_returns_none() {
        assert!(next_hint("item", &serde_json::Value::String("text".into())).is_none());
    }
}
