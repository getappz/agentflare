//! Request/response types and small shared helpers used across mcp_server.rs's
//! MCP tool handlers -- split out to shrink mcp_server.rs (see item #168).

use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct GetRoutingSuggestionRequest {
    #[schemars(description = "The user's prompt to analyze")]
    pub(crate) prompt: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct CheckSessionHealthRequest {
    #[schemars(description = "The session ID to check")]
    pub(crate) session_id: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct SkillDetectRequest {
    #[schemars(description = "The user prompt to classify and match against skills")]
    pub(crate) prompt: String,
    #[schemars(description = "Max skills to return (default 3)")]
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    #[schemars(
        description = "If true, return skill body content instead of just metadata (default false)"
    )]
    #[serde(default)]
    pub(crate) include_body: bool,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct SkillRequest {
    #[schemars(description = "Action: search|load")]
    pub(crate) action: String,
    #[schemars(description = "What you need to do; keyword-style works best (search)")]
    #[serde(default)]
    pub(crate) query: Option<String>,
    #[schemars(description = "Skill name; qualify as 'source:name' if ambiguous (load)")]
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[schemars(description = "Max results (default 5) (search)")]
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    #[schemars(
        description = "'all' = every word must match (default); 'any' = broader recall for retries (search)"
    )]
    #[serde(default)]
    pub(crate) mode: Option<String>,
    #[schemars(description = "true = load the original even when a compressed copy exists (load)")]
    #[serde(default)]
    pub(crate) original: bool,
    #[schemars(
        description = "Wrap skill body + siblings in a <SKILL_ACTIVATION> enforcement block (load only, default false)"
    )]
    #[serde(default)]
    pub(crate) activation_wrapper: bool,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct ToolRequest {
    #[schemars(description = "Action: search|execute")]
    pub(crate) action: String,
    #[schemars(description = "What tool you need; keyword-style works best (search)")]
    #[serde(default)]
    pub(crate) query: Option<String>,
    #[schemars(description = "Max results (default 5) (search)")]
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    #[schemars(
        description = "'all' = every word must match (default); 'any' = broader recall for retries (search)"
    )]
    #[serde(default)]
    pub(crate) mode: Option<String>,
    #[schemars(description = "Server name from the search action (execute)")]
    #[serde(default)]
    pub(crate) server: Option<String>,
    #[schemars(description = "Tool name from the search action (execute)")]
    #[serde(default)]
    pub(crate) tool: Option<String>,
    // A bare `serde_json::Value` here made schemars emit a typeless schema
    // (Value can be anything), so callers had no signal to send a nested
    // JSON object rather than a stringified one — execute couldn't actually
    // be invoked with arguments. `Map` renders as `{"type": ["object",
    // "null"]}`, a real hint.
    #[schemars(description = "Arguments object matching the tool's input_schema (execute)")]
    #[serde(default)]
    pub(crate) args: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct ClaimRequest {
    #[schemars(description = "Action: acquire|done|heartbeat|list|release")]
    pub(crate) action: String,
    #[schemars(
        description = "Target to claim, e.g. \"issue#42\", \"pr#7\", \"item#<uuid>\", or \"item#<seq_id>\""
    )]
    #[serde(default)]
    pub(crate) target: Option<String>,
    #[schemars(description = "Repo key owner/name (default: normalized origin remote)")]
    #[serde(default)]
    pub(crate) repo: Option<String>,
    #[schemars(description = "Include stale and done claims (default false) (list)")]
    #[serde(default)]
    pub(crate) all: bool,
    #[schemars(description = "List across every repo in the ledger (default false) (list)")]
    #[serde(default)]
    pub(crate) all_repos: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct ChannelSendRequest {
    #[schemars(description = "Platform to send to: telegram, slack, or discord")]
    pub(crate) platform: String,
    #[schemars(description = "Recipient id: Telegram chat_id, or Slack/Discord channel id")]
    pub(crate) target: String,
    #[schemars(description = "The message text to send")]
    pub(crate) message: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct ReviewRequest {
    #[schemars(description = "Action: clear|consensus|list|record|scores|submit")]
    pub(crate) action: String,
    #[schemars(
        description = "Findings, each {file, line, message, severity?, category?} (submit)"
    )]
    #[serde(default)]
    pub(crate) findings: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Review round id (default: current branch)")]
    #[serde(default)]
    pub(crate) pr: Option<String>,
    #[schemars(description = "Finder name (default: detected agent) (submit)")]
    #[serde(default)]
    pub(crate) agent: Option<String>,
    #[schemars(description = "Diff base ref (default: master) (consensus, record)")]
    #[serde(default)]
    pub(crate) base: Option<String>,
    #[schemars(description = "Diff head ref (default: HEAD) (consensus, record)")]
    #[serde(default)]
    pub(crate) head: Option<String>,
    #[schemars(description = "Repo key owner/name (default: origin remote)")]
    #[serde(default)]
    pub(crate) repo: Option<String>,
    #[schemars(description = "Aggregate across every repo (default false) (scores)")]
    #[serde(default)]
    pub(crate) all_repos: bool,
}

/// A handoff assigns an item to another agent and attaches the work product
/// to it as an asset. Unlike a bare item update, `recipient` is a required
/// field, not `Option` — the schema itself makes an unaddressed handoff
/// unrepresentable, so an intended handoff can't silently land with no
/// assignee. Re-attaching under the same `item_id` (or the same generated
/// filename on a freshly created item) becomes the next asset version, not
/// a duplicate.
#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct HandoffRequest {
    #[schemars(
        description = "Agent/runtime this handoff is addressed to — becomes the item's assignee_agent. Required."
    )]
    pub(crate) recipient: String,
    #[schemars(
        description = "Short name/brief for the handoff — the item's name when creating one"
    )]
    pub(crate) name: String,
    #[schemars(
        description = "The work product being handed off (diff, review, document, ...). Prepend the brief so the recipient knows the ask. Attached to the item as an asset."
    )]
    pub(crate) content: String,
    #[schemars(
        description = "html | markdown | mermaid | diagram | text (default: markdown) — picks the attached asset's extension/mime type"
    )]
    #[serde(default)]
    pub(crate) r#type: Option<String>,
    #[schemars(
        description = "Existing item ID to assign and attach to, instead of creating a new one. If the work already has a home item, always pass its id here — omitting it unconditionally creates a new item, even when one covering this work already exists."
    )]
    #[serde(default)]
    pub(crate) item_id: Option<String>,
    #[schemars(
        description = "Handoff thread to continue; omit to start a new one. Stored in the new item's metadata, or the attached asset's metadata when item_id is given."
    )]
    #[serde(default)]
    pub(crate) thread_id: Option<String>,
    #[schemars(
        description = "Id this replies to (when answering an inbox item) — stored in the attached asset's metadata for provenance"
    )]
    #[serde(default)]
    pub(crate) reply_to: Option<String>,
    #[schemars(
        description = "One-line description; used as the new item's description when creating one"
    )]
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[schemars(
        description = "Knowledge facts to import into the recipient's memory on receive. Each item: {title, content, type, topic_key?}."
    )]
    #[serde(default)]
    pub(crate) facts: Option<Vec<serde_json::Value>>,
    #[schemars(
        description = "Sender's session summary — embedded in the handoff so the recipient sees context without extra round-trips."
    )]
    #[serde(default)]
    pub(crate) summary: Option<String>,
    #[schemars(description = "Findings array [{file, line?, summary}] — session snapshot.")]
    #[serde(default)]
    pub(crate) findings: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Decisions array [{summary, rationale?}] — session snapshot.")]
    #[serde(default)]
    pub(crate) decisions: Option<Vec<serde_json::Value>>,
    #[schemars(
        description = "Files touched array [{path, modified?, tokens}] — session snapshot."
    )]
    #[serde(default)]
    pub(crate) files_touched: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Evidence array [{kind, action, detail}] — session snapshot.")]
    #[serde(default)]
    pub(crate) evidence: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct ArtifactRequest {
    #[schemars(description = "Action: delete|diff|get|list|publish|search")]
    pub(crate) action: String,
    #[schemars(description = "Artifact id")]
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[schemars(description = "Display name of the artifact (publish)")]
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[schemars(
        description = "html | markdown | mermaid | diagram | text (default: text) (publish)"
    )]
    #[serde(default)]
    pub(crate) r#type: Option<String>,
    #[schemars(
        description = "Full artifact content (HTML document, markdown source, plain text, ...) (publish)"
    )]
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[schemars(description = "Session ID for grouping artifacts (optional)")]
    #[serde(default)]
    pub(crate) session_id: Option<String>,
    #[schemars(
        description = "Existing artifact id to update in place — keeps the same URL and live-reloads open viewers (publish)"
    )]
    #[serde(default)]
    pub(crate) update_id: Option<String>,
    #[schemars(
        description = "Short label for this version, shown in history (e.g. \"draft\", \"final\") (publish)"
    )]
    #[serde(default)]
    pub(crate) label: Option<String>,
    #[schemars(description = "One-line description shown in the gallery (publish)")]
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[schemars(description = "One or two emoji used as the page icon (publish)")]
    #[serde(default)]
    pub(crate) favicon: Option<String>,
    #[schemars(
        description = "Optimistic-concurrency guard: update only applies if the artifact's current version equals this; otherwise a version-conflict error is returned (publish)"
    )]
    #[serde(default)]
    pub(crate) base_version: Option<u32>,
    #[schemars(
        description = "Handoff envelope: agent/runtime this artifact is addressed to — for WORK PRODUCTS only; facts and decisions belong in memory (memory_remember), not artifacts (publish)"
    )]
    #[serde(default)]
    pub(crate) recipient: Option<String>,
    #[schemars(
        description = "Handoff envelope: thread this belongs to; replies reuse the sender's thread_id (publish)"
    )]
    #[serde(default)]
    pub(crate) thread_id: Option<String>,
    #[schemars(description = "Handoff envelope: artifact id this replies to (publish)")]
    #[serde(default)]
    pub(crate) reply_to: Option<String>,
    #[schemars(description = "Older version number to diff from (diff)")]
    #[serde(default)]
    pub(crate) from_version: Option<u32>,
    #[schemars(description = "Newer version number (omit for latest) (diff)")]
    #[serde(default)]
    pub(crate) to_version: Option<u32>,
    #[schemars(
        description = "Case-insensitive text to find in names, descriptions, or content (search)"
    )]
    #[serde(default)]
    pub(crate) query: Option<String>,
    #[schemars(description = "Specific version to fetch (omit for latest) (get)")]
    #[serde(default)]
    pub(crate) version: Option<u32>,
    #[schemars(
        description = "Inbox filter: only artifacts addressed to this agent/runtime (list)"
    )]
    #[serde(default)]
    pub(crate) inbox_recipient: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct OptimizeRequest {
    #[schemars(description = "Action: retrieve | list")]
    pub(crate) action: String,
    #[serde(default)]
    #[schemars(description = "Registered compression id (required for retrieve)")]
    pub(crate) id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct VentRequest {
    pub(crate) message: String,
    pub(crate) severity: Option<String>,
    pub(crate) tags: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct MemoryRequest {
    #[schemars(description = "Action: compact|context|curate|handoff|recall|relate|remember")]
    pub(crate) action: String,
    #[schemars(description = "Title of the observation (remember)")]
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[schemars(description = "Content body of the observation (remember, curate)")]
    #[serde(default)]
    pub(crate) content: Option<String>,
    #[schemars(
        description = "Type: decision|bugfix|discovery|pattern|learning|manual (remember, recall)"
    )]
    #[serde(default)]
    pub(crate) r#type: Option<String>,
    #[schemars(description = "Session ID to associate with")]
    #[serde(default)]
    pub(crate) session_id: Option<String>,
    #[schemars(description = "Project name")]
    #[serde(default)]
    pub(crate) project: Option<String>,
    #[schemars(description = "Stable topic key for upsert dedup (remember)")]
    #[serde(default)]
    pub(crate) topic_key: Option<String>,
    #[schemars(description = "Scope: project (default) or personal (remember)")]
    #[serde(default)]
    pub(crate) scope: Option<String>,
    #[schemars(description = "Search query (FTS5 BM25); omit for recent listing (recall)")]
    #[serde(default)]
    pub(crate) query: Option<String>,
    #[schemars(description = "Direct lookup by ID (recall)")]
    #[serde(default)]
    pub(crate) id: Option<i64>,
    #[schemars(description = "Max results (default 10, max 50) (recall)")]
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    #[schemars(description = "Session summary (handoff)")]
    #[serde(default)]
    pub(crate) summary: Option<String>,
    #[schemars(description = "Findings array [{file, line?, summary}] (handoff)")]
    #[serde(default)]
    pub(crate) findings: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Decisions array [{summary, rationale?}] (handoff)")]
    #[serde(default)]
    pub(crate) decisions: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Files touched array [{path, modified?, tokens}] (handoff)")]
    #[serde(default)]
    pub(crate) files_touched: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Evidence array [{kind, action, detail}] (handoff)")]
    #[serde(default)]
    pub(crate) evidence: Option<Vec<serde_json::Value>>,
    #[schemars(description = "Source observation ID (relate)")]
    #[serde(default)]
    pub(crate) source_id: Option<i64>,
    #[schemars(description = "Target observation ID (relate)")]
    #[serde(default)]
    pub(crate) target_id: Option<i64>,
    #[schemars(
        description = "Relation: related|compatible|scoped|conflicts_with|supersedes|not_conflict (relate)"
    )]
    #[serde(default)]
    pub(crate) relation: Option<String>,
    #[schemars(description = "Reason for the relation (relate)")]
    #[serde(default)]
    pub(crate) reason: Option<String>,
    #[schemars(description = "Confidence score 0.0..1.0 (relate)")]
    #[serde(default)]
    pub(crate) confidence: Option<f64>,
    #[schemars(description = "Pin status (curate pin/unpin actions)")]
    #[serde(default)]
    pub(crate) pinned: Option<bool>,
    #[schemars(description = "Sub-action for curate: update|delete|pin|unpin")]
    #[serde(default)]
    pub(crate) curate_action: Option<String>,
    #[schemars(description = "Target fraction of lines to keep (0.0-1.0, compact)")]
    #[serde(default)]
    pub(crate) compression_ratio: Option<f64>,
    #[schemars(description = "Keep N most recent messages verbatim (compact)")]
    #[serde(default)]
    pub(crate) preserve_recent: Option<usize>,
    #[schemars(description = "Scorer backend: fts5 (compact)")]
    #[serde(default)]
    pub(crate) scorer: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct GitHubRequest {
    #[schemars(
        description = "Action: pr_create|pr_list|pr_get|pr_status|pr_merge|pr_comment|pr_request_review|issue_create|issue_list|issue_get|issue_comment|issue_close|issue_label|release_list|release_get|release_latest|release_create|run_list|run_get|run_rerun|workflow_dispatch"
    )]
    pub(crate) action: String,
    #[schemars(description = "owner/repo (default: resolved from the current repo's origin)")]
    #[serde(default)]
    pub(crate) repo: Option<String>,
    #[schemars(
        description = "PR number (pr_get, pr_status, pr_merge, pr_comment, pr_request_review)"
    )]
    #[serde(default)]
    pub(crate) number: Option<u64>,
    #[schemars(description = "PR title (pr_create)")]
    #[serde(default)]
    pub(crate) title: Option<String>,
    #[schemars(description = "Head branch (pr_create)")]
    #[serde(default)]
    pub(crate) head: Option<String>,
    #[schemars(description = "Base branch (pr_create)")]
    #[serde(default)]
    pub(crate) base: Option<String>,
    #[schemars(description = "Body / comment text (pr_create, pr_comment)")]
    #[serde(default)]
    pub(crate) body: Option<String>,
    #[schemars(description = "State filter for pr_list: open|closed|all (default open)")]
    #[serde(default)]
    pub(crate) state: Option<String>,
    #[schemars(description = "Merge method for pr_merge: merge|squash|rebase (default merge)")]
    #[serde(default)]
    pub(crate) merge_method: Option<String>,
    #[schemars(description = "Reviewer logins (pr_request_review)")]
    #[serde(default)]
    pub(crate) reviewers: Option<Vec<String>>,
    #[schemars(description = "Labels (issue_create, issue_label)")]
    #[serde(default)]
    pub(crate) labels: Option<Vec<String>>,
    #[schemars(description = "Assignee logins (issue_create)")]
    #[serde(default)]
    pub(crate) assignees: Option<Vec<String>>,
    #[schemars(description = "Release id (release_get)")]
    #[serde(default)]
    pub(crate) release_id: Option<u64>,
    #[schemars(description = "Git tag (release_create)")]
    #[serde(default)]
    pub(crate) tag: Option<String>,
    #[schemars(description = "Release name (release_create)")]
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[schemars(description = "Mark release as draft (release_create, default false)")]
    #[serde(default)]
    pub(crate) draft: Option<bool>,
    #[schemars(description = "Mark release as prerelease (release_create, default false)")]
    #[serde(default)]
    pub(crate) prerelease: Option<bool>,
    #[schemars(description = "Workflow run id (run_get, run_rerun)")]
    #[serde(default)]
    pub(crate) run_id: Option<u64>,
    #[schemars(description = "Branch filter for run_list")]
    #[serde(default)]
    pub(crate) branch: Option<String>,
    #[schemars(description = "Workflow file name or id (workflow_dispatch)")]
    #[serde(default)]
    pub(crate) workflow: Option<String>,
    #[schemars(
        description = "Git ref to dispatch against (workflow_dispatch, default: repo default branch)"
    )]
    #[serde(default)]
    pub(crate) git_ref: Option<String>,
    #[schemars(description = "JSON inputs object for workflow_dispatch")]
    #[serde(default)]
    pub(crate) inputs: Option<serde_json::Value>,
    #[schemars(
        description = "ISO8601 timestamp — pr_status only returns comments newer than this (GitHub filters server-side); omit for full history"
    )]
    #[serde(default)]
    pub(crate) since: Option<String>,
}

/// All local artifact backends (flared, another session, or our own
/// owned server) bind loopback-only — never advertise anything else.
pub(crate) const LOCAL_HOST: &str = "127.0.0.1";
/// flared's default HTTP port; its artifact routes live under /artifacts.
pub(crate) const FLARED_DEFAULT_PORT: u16 = 35273;
pub(crate) const FLARED_ARTIFACTS_PATH: &str = "/artifacts/";

/// flared's HTTP port: honor a `port` override in its config.toml when
/// readable (a `--port` CLI override is invisible here and lands on the
/// fixed-port fallback chain); default otherwise.
pub(crate) fn flared_port() -> u16 {
    dirs::config_dir()
        .map(|dir| dir.join("flared").join("config.toml"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| parse_flared_port(&text))
        .unwrap_or(FLARED_DEFAULT_PORT)
}

/// Extract the top-level `port` key from flared's config.toml text — a
/// minimal scan that avoids a toml dependency for one key. Absent or
/// malformed values -> None.
pub(crate) fn parse_flared_port(text: &str) -> Option<u16> {
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
pub(crate) enum ArtifactBackend {
    /// This process owns the listener.
    Owned(agentflare_artifacts::ArtifactServer),
    /// Another process serves the shared store: flared under /artifacts on
    /// its fixed port, or an earlier session's root-mounted server.
    External { port: u16, path: &'static str },
}

impl ArtifactBackend {
    /// Base URL artifact links hang off (no trailing slash).
    pub(crate) fn base_url(&self) -> String {
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
pub(crate) struct ProjectLink {
    pub(crate) workspace_id: String,
    pub(crate) project_id: String,
    pub(crate) identifier: String,
}

/// Default 4h — item claims are plausibly longer-running than
/// `src/claims.rs`'s 30-min GitHub-issue-claim default, hence a separate env
/// var rather than sharing `AGENTFLARE_CLAIM_TTL_SECS`.
pub(crate) fn backend_claim_ttl_secs() -> i64 {
    std::env::var("AGENTFLARE_BACKEND_CLAIM_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(14400) as i64
}

/// NotFound/Duplicate/InvalidTransition are caller-fixable → invalid_params;
/// a raw database error is ours to fix → internal_error. Same split as
/// `skill_load`'s NotFound/Ambiguous handling above.
pub(crate) fn map_backend_err(e: agentflare_backend::Error) -> ErrorData {
    match e {
        agentflare_backend::Error::NotFound(msg)
        | agentflare_backend::Error::Duplicate(msg)
        | agentflare_backend::Error::InvalidTransition(msg)
        | agentflare_backend::Error::Validation(msg) => ErrorData::invalid_params(msg, None),
        agentflare_backend::Error::Database(e) => ErrorData::internal_error(e.to_string(), None),
    }
}

/// Maps a `GitHubError` to MCP `ErrorData`: client/auth mistakes become
/// `invalid_params`, transport/parse failures become `internal_error`.
pub(crate) fn to_mcp_error(err: crate::github::GitHubError) -> ErrorData {
    let msg = err.to_string();
    if crate::github::mcp::is_client_error(&err) {
        ErrorData::invalid_params(msg, None)
    } else {
        ErrorData::internal_error(msg, None)
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
pub(crate) fn generate_webhook_secret() -> String {
    use rand::Rng;
    let bytes: [u8; 24] = rand::thread_rng().r#gen();
    hex::encode(bytes)
}

pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    use base64::engine::general_purpose;
    general_purpose::STANDARD.encode(bytes)
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct ItemRequest {
    #[schemars(
        description = "Action: create|get|list|search|update|update_state|delete|claim|heartbeat|release|done|cancel|add_label|remove_label|groom|standup|health"
    )]
    pub(crate) action: String,
    #[schemars(
        description = "Item ID (UUID or numeric sequence_id) — required for get, update, update_state, delete, claim, heartbeat, release, done, add_label, remove_label"
    )]
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[schemars(description = "Item name/title (required for create)")]
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[schemars(
        description = "State ID (create, update_state); omit to use the project's default (Backlog) state"
    )]
    #[serde(default)]
    pub(crate) state_id: Option<String>,
    #[schemars(description = "Markdown description body (create, update)")]
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[schemars(description = "Priority: none|low|medium|high|urgent (create, update)")]
    #[serde(default)]
    pub(crate) priority: Option<String>,
    #[schemars(description = "Parent item ID, for sub-items (create)")]
    #[serde(default)]
    pub(crate) parent_id: Option<String>,
    #[schemars(
        description = "Agent ID to assign (create, update), or to filter by (list — matches items assigned to this agent plus unassigned ones, sorted open+assigned-to-you first)"
    )]
    #[serde(default)]
    pub(crate) assignee_agent: Option<String>,
    #[schemars(
        description = "Domain-specific fields as a JSON object (create, update). Set {\"size\": \"S\"|\"M\"|\"L\"} so `groom` can score effort instead of reporting the item unestimated."
    )]
    #[serde(default)]
    pub(crate) metadata: Option<serde_json::Value>,
    #[schemars(description = "Label IDs to attach on creation (create)")]
    #[serde(default)]
    pub(crate) label_ids: Option<Vec<String>>,
    #[schemars(description = "Item IDs this item depends on (create)")]
    #[serde(default)]
    pub(crate) dependency_ids: Option<Vec<String>>,
    #[schemars(description = "Label ID (add_label, remove_label)")]
    #[serde(default)]
    pub(crate) label_id: Option<String>,
    #[schemars(
        description = "Filter by state group (list); one of backlog|unstarted|started|completed|cancelled|triage, or a comma-separated list (e.g. \"backlog,unstarted,started\") to match any"
    )]
    #[serde(default)]
    pub(crate) state_group: Option<String>,
    #[schemars(
        description = "Max items to return (list: omit for no limit; search: omit for 20, capped at 1000; groom: omit for 15, capped at 200)"
    )]
    #[serde(default)]
    pub(crate) limit: Option<i64>,
    #[schemars(description = "Items to skip before applying limit (list); default 0")]
    #[serde(default)]
    pub(crate) offset: Option<i64>,
    #[schemars(description = "FTS5 search query (search)")]
    #[serde(default)]
    pub(crate) query: Option<String>,
    #[schemars(
        description = "Days since updated_at before an item counts as stale/stuck (groom: default 14; standup/health: default 7)"
    )]
    #[serde(default)]
    pub(crate) staleness_days: Option<i64>,
    #[schemars(
        description = "Now-bucket size (groom only) — when set, additionally buckets the shortlist into now/next/later/needs_estimation for sprint planning"
    )]
    #[serde(default)]
    pub(crate) capacity: Option<i64>,
    #[schemars(
        description = "Hours back a completed item counts as \"done\" (standup); default 24"
    )]
    #[serde(default)]
    pub(crate) cutoff_hours: Option<i64>,
    #[schemars(description = "Trailing weekly windows for velocity (health); default 4, max 52")]
    #[serde(default)]
    pub(crate) window_weeks: Option<i64>,
}

/// Lean per-item projection for `item(list)` — the raw 19-field `Item` (full
/// description/metadata/timestamps) is what `get` returns; `list` only needs
/// enough to triage, and resolves the opaque `state_id` into a readable name.
#[derive(Debug, serde::Serialize)]
pub(crate) struct ItemSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) state: String,
    pub(crate) state_group: String,
    pub(crate) priority: String,
    pub(crate) assignee_agent: Option<String>,
    pub(crate) parent_id: Option<String>,
    pub(crate) sequence_id: i64,
    pub(crate) updated_at: i64,
}

/// One shortlisted item plus the decision-support signals `groom` computes
/// server-side (staleness, blocking, fan-in, near-duplicates) so the caller
/// doesn't have to re-derive them by eyeballing timestamps and free text.
#[derive(Debug, serde::Serialize)]
pub(crate) struct GroomItem {
    pub(crate) id: String,
    pub(crate) sequence_id: i64,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) state: String,
    pub(crate) state_group: String,
    pub(crate) priority: String,
    pub(crate) assignee_agent: Option<String>,
    pub(crate) updated_at: i64,
    pub(crate) stale: bool,
    pub(crate) unassigned: bool,
    /// Parsed from `metadata.size` ("S"|"M"|"L"); `None` when absent — see `unestimated`.
    pub(crate) size: Option<String>,
    /// True when `metadata.size` is missing — add a size label to enable real RICE scoring.
    pub(crate) unestimated: bool,
    /// IDs this item depends on that are still open (not completed/cancelled).
    pub(crate) blocked_by: Vec<String>,
    /// How many other items declare a dependency on this one.
    pub(crate) depended_on_by_count: i64,
    /// Other shortlisted items with a near-identical name (token-Jaccard ≥ 0.5).
    pub(crate) possible_duplicates: Vec<String>,
}

/// One-call groom result: priority+staleness-ranked shortlist with all the
/// flags a human/agent needs to make pull-next decisions, computed in Rust
/// instead of costing N `get` round trips + manual LLM staleness/dup checks.
#[derive(Debug, serde::Serialize)]
pub(crate) struct GroomResponse {
    pub(crate) staleness_days: i64,
    pub(crate) stale_count: usize,
    pub(crate) unassigned_count: usize,
    pub(crate) unestimated_count: usize,
    pub(crate) items: Vec<GroomItem>,
    /// Top unassigned, not-stale, unblocked items from the shortlist.
    pub(crate) pull_next: Vec<String>,
    /// Only populated when the `capacity` request field is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) now: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) next: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) later: Option<Vec<String>>,
    /// Unestimated items — excluded from now/next/later, can't be planned yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) needs_estimation: Option<Vec<String>>,
}

/// Lean per-item row for `standup` — no description, matches `ItemSummary`'s
/// thin-projection philosophy since standup doesn't need item bodies.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct StandupItem {
    pub(crate) id: String,
    pub(crate) sequence_id: i64,
    pub(crate) name: String,
    pub(crate) priority: String,
    pub(crate) assignee_agent: Option<String>,
    pub(crate) updated_at: i64,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct StandupGroup {
    /// The literal string "unassigned" when `assignee_agent` is null.
    pub(crate) assignee: String,
    pub(crate) items: Vec<StandupItem>,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct StandupResponse {
    pub(crate) cutoff_hours: i64,
    pub(crate) stuck_days: i64,
    pub(crate) done: Vec<StandupItem>,
    pub(crate) done_count: usize,
    pub(crate) in_progress: Vec<StandupGroup>,
    pub(crate) in_progress_count: usize,
    pub(crate) stuck: Vec<StandupItem>,
    pub(crate) stuck_count: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct VelocityWeek {
    pub(crate) week_start: i64,
    pub(crate) week_end: i64,
    pub(crate) completed_count: usize,
}

#[derive(Debug, serde::Serialize)]
pub(crate) struct HealthResponse {
    pub(crate) window_weeks: i64,
    /// Oldest → newest.
    pub(crate) velocity: Vec<VelocityWeek>,
    /// "up" | "down" | "flat" — last window vs. the one before it.
    pub(crate) velocity_trend: String,
    pub(crate) wip_count: usize,
    pub(crate) wip: Vec<StandupItem>,
    pub(crate) stuck_days: i64,
    pub(crate) stuck_count: usize,
    pub(crate) stuck: Vec<StandupItem>,
    /// Empty today — agentflare has no persisted handoff log distinct from
    /// item state, so this can't be computed yet (see `bottleneck_note`).
    pub(crate) bottlenecks: Vec<String>,
    pub(crate) bottleneck_note: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct CommentRequest {
    #[schemars(description = "Action: create|edit|delete|list")]
    pub(crate) action: String,
    #[schemars(description = "Item ID to comment on (required for create, list)")]
    #[serde(default)]
    pub(crate) item_id: Option<String>,
    #[schemars(description = "Comment ID (required for edit, delete)")]
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[schemars(description = "Comment body text (required for create, edit)")]
    #[serde(default)]
    pub(crate) body: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct LabelRequest {
    #[schemars(description = "Action: create|list|update|delete")]
    pub(crate) action: String,
    #[schemars(description = "Label ID (required for update, delete)")]
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[schemars(description = "Label name (required for create; optional for update)")]
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[schemars(description = "Hex color, e.g. #F59E0B (create, update)")]
    #[serde(default)]
    pub(crate) color: Option<String>,
    #[schemars(description = "Parent label ID for nesting/grouping (create)")]
    #[serde(default)]
    pub(crate) parent_id: Option<String>,
    #[schemars(description = "Sort order for manual ordering (create, update)")]
    #[serde(default)]
    pub(crate) sort_order: Option<f64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct WebhookRequest {
    #[schemars(description = "Action: create|list|delete")]
    pub(crate) action: String,
    #[schemars(description = "Webhook ID (required for delete)")]
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[schemars(description = "HTTPS/HTTP URL to deliver events to (required for create)")]
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[schemars(description = "HMAC signing secret; auto-generated if omitted (create)")]
    #[serde(default)]
    pub(crate) secret: Option<String>,
    #[schemars(description = "Fire on item create/update/delete (create)")]
    #[serde(default)]
    pub(crate) on_item: Option<bool>,
    #[schemars(description = "Fire on state changes (create)")]
    #[serde(default)]
    pub(crate) on_state: Option<bool>,
    #[schemars(description = "Fire on project changes (create)")]
    #[serde(default)]
    pub(crate) on_project: Option<bool>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(crate) struct ProjectRequest {
    #[schemars(description = "Action: info")]
    pub(crate) action: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct AssetRequest {
    #[schemars(description = "Action: attach|get|list|delete")]
    pub(crate) action: String,
    #[schemars(description = "Asset ID (required for get, delete)")]
    #[serde(default)]
    pub(crate) id: Option<String>,
    #[schemars(description = "Item ID to attach to (xor project_id)")]
    #[serde(default)]
    pub(crate) item_id: Option<String>,
    #[schemars(description = "Project ID to attach to (xor item_id)")]
    #[serde(default)]
    pub(crate) project_id: Option<String>,
    #[schemars(
        description = "Filename (required for attach) — must exist in ~/.agentflare/staging/"
    )]
    #[serde(default)]
    pub(crate) filename: Option<String>,
    #[schemars(description = "JSON metadata (optional, attach only)")]
    #[serde(default)]
    pub(crate) metadata: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct SearchRequest {
    #[schemars(description = "Search query")]
    pub(crate) query: String,
    #[schemars(
        description = "Search type: 'store' (default, FTS across store documents, grouped by doc_type), 'memory' (FTS across brain.db observations), 'code' (gateway leanctx ctx_search), or 'web' (rivalsearch internet search)"
    )]
    #[serde(default)]
    pub(crate) r#type: Option<String>,
    #[schemars(description = "Max results (default 20; code 50, web 10)")]
    #[serde(default)]
    pub(crate) limit: Option<usize>,
}
