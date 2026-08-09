use crate::agent_launch::{self, HeadlessOutcome};
use crate::mcp_server::AgentflareMcp;
use crate::mcp_server::types::{CommentRequest, ItemRequest};
use agent_registry::{self, autonomous_args, headless_args};
use clap::Args;
use std::time::Duration;

/// `agentflare work --timeout`'s default, and what the in-process
/// [`WorkItemExecutor`] uses for daemon-dispatched work items (which don't
/// go through CLI arg parsing, so can't pick up clap's `default_value_t`) —
/// named so the two can't silently drift apart.
pub const DEFAULT_TIMEOUT_SECS: u64 = 21_600;
/// `agentflare work --idle-timeout`'s default; see [`DEFAULT_TIMEOUT_SECS`].
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;

/// Claim a work item, run an agent on it in an isolated worktree, and
/// report the result (comment + PR, or error) back onto the item.
#[derive(Args)]
pub struct WorkArgs {
    /// Item UUID or numeric sequence id.
    pub target: String,
    /// Agent to run (e.g. claude-code, codex, gemini-cli). Omit to let
    /// agentflare route automatically from the claimed item's assignee_agent
    /// and any `~/.agentflare/config.toml` `[router]` rules.
    #[arg(long)]
    pub agent: Option<String>,
    /// Absolute hard-cap timeout in seconds, regardless of activity
    /// (default 21600 = 6h). A backstop against a runaway process, not the
    /// primary signal for whether to keep a job alive — see --idle-timeout.
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    pub timeout: u64,
    /// Kill the agent if it produces no new stdout/stderr output for this
    /// many seconds (default 300 = 5 min). This is the primary liveness
    /// signal: a task that keeps producing output can run all the way to
    /// --timeout even if that takes hours; a genuinely stuck task is caught
    /// quickly instead of running out the full --timeout with nothing
    /// happening.
    #[arg(long, default_value_t = DEFAULT_IDLE_TIMEOUT_SECS)]
    pub idle_timeout: u64,
    /// Max agent turns before forced stop (Claude Code only).
    #[arg(long)]
    pub max_turns: Option<u64>,
    /// Max cost in USD before forced stop (Claude Code only).
    #[arg(long)]
    pub max_cost_usd: Option<f64>,
    /// Channel recipient for a handoff artifact on outcome.
    #[arg(long)]
    pub notify: Option<String>,
}

/// Builds the agent prompt from the item's name/description plus any prior
/// discussion, so a resumed/re-run worker sees what's already been tried.
fn build_prompt(
    item: &agentflare_backend::item::Item,
    comments: &[agentflare_backend::comment::ItemComment],
) -> String {
    let mut prompt = format!(
        "Work item #{} — {}\n\n{}\n",
        item.sequence_id, item.name, item.description
    );
    if !comments.is_empty() {
        prompt.push_str("\nPrior discussion:\n");
        for c in comments {
            prompt.push_str(&format!("- [{}] {}\n", c.author_agent, c.body));
        }
    }
    prompt.push_str("\nWhen you are done, summarize what you changed and why.\n");
    prompt
}

/// Claude Code's `--output-format stream-json` reply shape: one JSON object
/// per line (system init, tool_use/tool_result, assistant messages, ...),
/// with only the FINAL line carrying `{"result": "...", "session_id": "...",
/// "total_cost_usd": 0.0}` — the same shape the single-object `json` format
/// uses for its one and only line, so parsing "the last line" handles both.
/// Falls back to the raw text unparsed for any agent/output whose last line
/// isn't that exact JSON shape — never errors, never blocks the caller.
fn parse_claude_reply(raw: &str) -> (String, Option<String>, Option<f64>) {
    let last_line = raw.trim().lines().next_back().unwrap_or("");
    match serde_json::from_str::<serde_json::Value>(last_line) {
        Ok(v) => {
            let text = v
                .get("result")
                .and_then(|r| r.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| raw.to_string());
            let session_id = v
                .get("session_id")
                .and_then(|s| s.as_str())
                .map(str::to_string);
            let cost = v.get("total_cost_usd").and_then(serde_json::Value::as_f64);
            (text, session_id, cost)
        }
        Err(_) => (raw.to_string(), None, None),
    }
}

fn format_success_comment(
    reply: &str,
    session_id: Option<&str>,
    cost_usd: Option<f64>,
    pr_url: Option<&str>,
) -> String {
    let mut body = format!("## agentflare work — complete\n\nAgent reply:\n\n```\n{reply}\n```");
    if let Some(url) = pr_url {
        body.push_str(&format!("\n\nPR: {url}"));
    }
    if session_id.is_some() || cost_usd.is_some() {
        body.push_str("\n\n---\n");
        if let Some(id) = session_id {
            body.push_str(&format!("session: {id}\n"));
        }
        if let Some(c) = cost_usd {
            body.push_str(&format!("cost: ${c:.4}\n"));
        }
    }
    body
}

fn failure_message(outcome: &HeadlessOutcome) -> String {
    match outcome {
        HeadlessOutcome::UnknownAgent(m)
        | HeadlessOutcome::NotHeadless(m)
        | HeadlessOutcome::NotFound(m)
        | HeadlessOutcome::Failed(m) => m.clone(),
        HeadlessOutcome::Ok(_) => {
            unreachable!("Ok is handled by the success path, never passed here")
        }
    }
}

/// Per-agent extra argv inserted before the prompt: the confirmed
/// permission-bypass flag, plus — Claude Code only, since it's the only
/// agent with a confirmed structured-output flag and native turn/cost caps
/// — `--output-format stream-json` (plus the `--verbose` Claude Code
/// requires alongside it — confirmed by hand: omitting it errors with
/// "--print with stream-json output requires --verbose") and any
/// `--max-turns`/`--max-cost-usd` the caller asked for. Other agents get
/// only their bypass flag; a caller-supplied cap for them is dropped with a
/// warning rather than guessed at.
///
/// NOT plain `--output-format json`: that format writes nothing to
/// stdout/stderr until the entire run finishes (confirmed by hand: 0 bytes
/// for 54s+ on a trivial 2-tool-call task), so `run_captured`'s idle-timeout
/// (default 300s) kills any real, longer task as a false-positive hang
/// before it can ever finish — the actual cause behind item #43's repeated
/// "went idle for 300s (no output captured)" failures. `stream-json` emits
/// one JSON object per turn/tool-call as it happens, giving genuine
/// liveness; `parse_claude_reply` reads the result back off its final line.
fn build_extra_args(
    agent: agent_registry::Agent,
    max_turns: Option<u64>,
    max_cost_usd: Option<f64>,
) -> Vec<String> {
    let mut args: Vec<String> = autonomous_args(agent)
        .into_iter()
        .flatten()
        .map(|s| s.to_string())
        .collect();
    if agent == agent_registry::Agent::ClaudeCode {
        args.push("--output-format".to_string());
        args.push("stream-json".to_string());
        args.push("--verbose".to_string());
        if let Some(turns) = max_turns {
            args.push(format!("--max-turns={turns}"));
        }
        if let Some(cost) = max_cost_usd {
            args.push(format!("--max-budget-usd={cost}"));
        }
    } else if max_turns.is_some() || max_cost_usd.is_some() {
        crate::ui::warning(
            "--max-turns/--max-cost-usd are only supported for claude-code currently — ignored",
        );
    }
    args
}

/// `~/.agentflare/config.toml`'s `[router]` table, or the empty config
/// (no rules, no default) when the file is missing — the common case until
/// someone actually writes one. A file that exists but can't be read (e.g.
/// a permission error) or fails to parse is a real misconfiguration, so
/// those print a warning rather than staying silent; either way
/// `agentflare work` degrades to requiring `--agent` instead of crashing
/// over a bad file.
fn load_router_config() -> agent_registry::RouterConfig {
    let path = crate::paths::home().join(".agentflare").join("config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return agent_registry::RouterConfig::default();
        }
        Err(e) => {
            crate::ui::warning(&format!(
                "{}: could not read config ({e}) — ignoring, pass --agent explicitly",
                path.display()
            ));
            return agent_registry::RouterConfig::default();
        }
    };
    match agent_registry::parse_router_config(&text) {
        Ok(config) => config,
        Err(e) => {
            crate::ui::warning(&format!(
                "{}: invalid [router] config ({e}) — ignoring, pass --agent explicitly",
                path.display()
            ));
            agent_registry::RouterConfig::default()
        }
    }
}

/// Resolves which agent runs this item, and why — the reason is printed so
/// an unexpected pick is self-explaining rather than just a bare agent
/// name. An explicit `--agent` always wins; otherwise `agent_registry::route`
/// decides from the item's own attributes against `config`
/// (`~/.agentflare/config.toml`'s `[router]` table, via
/// [`load_router_config`] — empty until that file exists). Even with no
/// rules configured, the item's own `assignee_agent` can still produce a
/// decision — a human already named one; everything else falls through to
/// the `Err` telling the caller to pass `--agent` explicitly.
fn resolve_agent(
    explicit: Option<&str>,
    item: &agentflare_backend::item::Item,
    labels: &[String],
    config: &agent_registry::RouterConfig,
    installed: &[agent_registry::Agent],
) -> Result<(agent_registry::Agent, String), String> {
    if let Some(name) = explicit {
        return agent_registry::agent_by_name(name)
            .map(|agent| (agent, "explicit --agent flag".to_string()))
            .ok_or_else(|| format!("unknown agent: {name} — use `agentflare agents list`"));
    }

    // `assignee_agent` may carry an instance suffix (`<agent>:<instance>`)
    // once the item has been claimed at least once — `item::claim` stores
    // the raw claim owner there deliberately (see its doc comment). Strip it
    // via the same `agent_part` the claim/handoff-freeze logic already uses
    // internally, so a previously-claimed item still routes to its own
    // assignee instead of silently falling through to the router's other
    // rules.
    let assigned_agent = item
        .assignee_agent
        .as_deref()
        .map(agentflare_backend::item::agent_part)
        .as_deref()
        .and_then(agent_registry::agent_by_name);
    let task = agent_registry::TaskContext {
        labels: labels.to_vec(),
        kind: crate::mcp_server::item::parsed_kind(&item.metadata),
        size: crate::mcp_server::item::parsed_size(&item.metadata),
        repo: None,
        assigned_agent,
    };
    agent_registry::route(&task, config, installed)
        .map(|decision| (decision.agent, decision.reason))
        .ok_or_else(|| {
            "no --agent given, and no route decision (item has no assignee and no router \
             rule matched) — pass --agent explicitly"
                .to_string()
        })
}

/// Releases the claim and posts a failure comment (+ optional handoff
/// notify) — the single path every early-exit and headless-failure branch
/// in `run_work` routes through, so a claimed item never dead-ends silently
/// held by a worker that errored out.
fn release_and_comment(
    mcp: &AgentflareMcp,
    item_id: &str,
    reason: &str,
    notify_recipient: Option<&str>,
) {
    let _ = mcp.item_release(ItemRequest {
        action: "release".into(),
        id: Some(item_id.into()),
        ..Default::default()
    });
    let comment_body = format!("## agentflare work — failed\n\n{reason}");
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item_id.into()),
        body: Some(comment_body.clone()),
        ..Default::default()
    });
    if let Some(recipient) = notify_recipient {
        notify(recipient, &comment_body, item_id);
    }
}

fn notify(recipient: &str, body: &str, item_id: &str) {
    let outcome = crate::cli::handoff::HandoffArgs {
        recipient: recipient.to_string(),
        file: None,
        content: Some(body.to_string()),
        thread: None,
        reply_to: None,
        name: Some(format!("item-{item_id}-result")),
        session: "handoffs".to_string(),
        sender: None,
        dir: None,
    }
    .publish();
    if let Err(e) = outcome {
        crate::ui::warning(&format!("notify {recipient} failed: {e}"));
    }
}

impl WorkArgs {
    pub fn run(self) {
        std::process::exit(execute_work(self, &mut std::io::stdout()).exit_code);
    }
}

/// `execute_work`'s result: the process exit code (0 = success), plus — set
/// only when the failure was classified as rate-limit shaped — a hint for
/// how long the job queue should wait before retrying this item.
/// `WorkItemExecutor` converts this into `agentflare_jobs::JobFailure`.
pub(crate) struct WorkOutcome {
    pub exit_code: i32,
    pub retry_after_secs: Option<u64>,
}

impl From<i32> for WorkOutcome {
    fn from(exit_code: i32) -> Self {
        WorkOutcome {
            exit_code,
            retry_after_secs: None,
        }
    }
}

/// Cooldown-table key used when there's no active vault rotation profile for
/// `agent` — keeps `auth_db`'s `(agent, profile)`-keyed cooldown table as the
/// single source of truth for both the interactive (`auth_runner`) and
/// autonomous (this file) dispatch paths, even for the common single-
/// credential setup that never configured vault profiles.
const DEFAULT_COOLDOWN_PROFILE: &str = "__default__";
/// Matches the cooldown length `auth_runner::run` already uses for the
/// interactive path's rate-limit rotation.
const RATE_LIMIT_COOLDOWN_MINUTES: u32 = 30;

/// Classifies a headless run's failure message the same way the interactive
/// `agentflare run` path does (`auth_runner::is_rate_limited`) and, if it
/// looks rate-limit shaped, records a cooldown so `auth_db::is_cooling_down`
/// (checked by the discovery tick before dispatching the next item for this
/// agent, and by `auth rotate`) sees it too. Returns the seconds until that
/// cooldown clears, for the caller to pass through as the job queue's
/// retry-after delay.
fn classify_and_cooldown(agent: &str, failure_message: &str) -> Option<u64> {
    if !crate::auth_runner::is_rate_limited(failure_message) {
        return None;
    }
    let conn = crate::auth_db::open_or_rebuild();
    let profile = crate::auth_db::get_rotation_last(&conn, agent)
        .map(|(profile, _)| profile)
        .unwrap_or_else(|| DEFAULT_COOLDOWN_PROFILE.to_string());
    crate::auth_db::set_cooldown(
        &conn,
        agent,
        &profile,
        RATE_LIMIT_COOLDOWN_MINUTES,
        "rate limit",
    );
    Some(RATE_LIMIT_COOLDOWN_MINUTES as u64 * 60)
}

/// Claims `args.target`, runs the resolved agent on it, and reports the
/// outcome back onto the item — the whole body of `agentflare work`.
/// Progress lines that used to go straight to stdout now go through `log`
/// instead, so this same logic can run in-process inside the daemon
/// (`WorkItemExecutor`, called from `agentflare_jobs::WorkerPool`) with its
/// progress captured into that job's own log file — the exact same file the
/// dashboard already tails for subprocess-dispatched jobs — rather than only
/// working when there's a real subprocess's stdout to capture.
pub(crate) fn execute_work(args: WorkArgs, log: &mut dyn std::io::Write) -> WorkOutcome {
    let mcp = AgentflareMcp::default();
    let timeout = Duration::from_secs(args.timeout);
    let idle_timeout = Duration::from_secs(args.idle_timeout);

    // An explicit --agent fails fast, before claiming anything, through the
    // same resolver `resolve_agent` uses further down post-claim — so this
    // check and the real resolution can never diverge. Auto-routing needs
    // the claimed item's own attributes, so it's resolved further down.
    if let Some(explicit) = args.agent.as_deref() {
        let Some(resolved) = agent_registry::agent_by_name(explicit) else {
            crate::ui::error(&format!(
                "unknown agent: {explicit} — use `agentflare agents list`"
            ));
            return 1.into();
        };
        // The claim below identifies its own owner via `claims::owner_id()`,
        // which falls back to agent-detector's parent-process/env sniffing
        // when AGENTFLARE_AGENT isn't set. That sniffing finds nothing when
        // this process is spawned headless (e.g. by the supervisor's
        // dispatch job, no parent agent process, no session env) and falls
        // back further to owner "cli" — which then loses to `item::claim`'s
        // BlockedByAssignee check against whatever agent the item was
        // actually assigned/dispatched to. An explicit `--agent` is a
        // stronger, unambiguous statement of identity than any of that
        // sniffing, so it wins outright here, same for a human typing it
        // directly or the supervisor dispatching this exact command.
        //
        // Skipped when a per-thread owner override is already active
        // (`WorkItemExecutor`'s in-process path, see `claims::owner_id()`):
        // that already makes `owner_id()` resolve correctly, and mutating
        // this process-global env var from a worker thread would race
        // against every other worker thread doing the same for a different
        // job — see `claims::with_owner_override`'s doc comment.
        //
        // SAFETY: when reached, this is a single-process-per-command CLI
        // invocation (no owner override active) — set once, synchronously,
        // before any worker threads exist in this process (this is the
        // first thing `execute_work` does).
        if !crate::claims::has_owner_override() {
            unsafe {
                std::env::set_var("AGENTFLARE_AGENT", resolved.as_str());
            }
        }
    }

    // --- Claim ---
    let claim_resp = match mcp.item_claim(ItemRequest {
        action: "claim".into(),
        id: Some(args.target.clone()),
        ..Default::default()
    }) {
        Ok(json) => json,
        Err(e) => {
            crate::ui::error(&format!("claim failed: {}", e.message));
            return 1.into();
        }
    };
    let claim: serde_json::Value =
        serde_json::from_str(&claim_resp).unwrap_or(serde_json::Value::Null);
    let status = claim["status"].as_str().unwrap_or("unknown");
    if status != "acquired" {
        let owner = claim["owner"].as_str().unwrap_or("?");
        let age = claim["age_secs"].as_i64().unwrap_or(0);
        crate::ui::error(&format!("item held by {owner} ({age}s) — cannot claim"));
        return 1.into();
    }
    let item_id = claim["item_id"]
        .as_str()
        .unwrap_or(&args.target)
        .to_string();
    let item_id = item_id.as_str();
    let _ = writeln!(log, "claimed: {item_id}");

    // --- Worktree ---
    let worktree_path = claim["worktree_path"]
        .as_str()
        .map(std::path::PathBuf::from);
    let Some(ref wpath) = worktree_path else {
        let msg = "claim succeeded but no worktree was created (bad git state?)";
        release_and_comment(&mcp, item_id, msg, args.notify.as_deref());
        crate::ui::error(msg);
        return 1.into();
    };
    let _ = writeln!(log, "worktree: {}", wpath.display());

    // --- Fetch item + prior discussion + labels ---
    let fetched = mcp.with_backend_db(|conn| {
        let resolved = mcp.resolve_item_id(conn, item_id).ok()?;
        let item = agentflare_backend::item::get(conn, &resolved).ok()?;
        let comments = agentflare_backend::comment::list_by_item(conn, &resolved).ok()?;
        let label_ids = agentflare_backend::item::list_labels(conn, &resolved).unwrap_or_default();
        let labels = label_ids
            .iter()
            .filter_map(|id| agentflare_backend::label::get(conn, id).ok())
            .map(|l| l.name)
            .collect::<Vec<_>>();
        Some((item, comments, labels))
    });
    let (item_detail, comments, labels) = match fetched {
        Ok(Some(triple)) => triple,
        _ => {
            let msg = "failed to read item details after claim";
            release_and_comment(&mcp, item_id, msg, args.notify.as_deref());
            crate::ui::error(msg);
            return 1.into();
        }
    };

    // --- Resolve agent (explicit flag, else route from the item) ---
    // Only pay for detecting installed agents / loading the router config
    // when there's no explicit --agent to short-circuit resolve_agent with.
    let (installed, router_config) = if args.agent.is_none() {
        let mut state = crate::state::load();
        let installed: Vec<agent_registry::Agent> = agent_registry::detect_all_with(
            agent_registry::REGISTRY,
            &mut state.version_cache,
            &agent_registry::RealVersionRunner,
        )
        .iter()
        .filter_map(|d| agent_registry::agent_by_name(d.id))
        .collect();
        crate::state::save(&state);
        (installed, load_router_config())
    } else {
        (Vec::new(), agent_registry::RouterConfig::default())
    };
    let (agent_enum, route_reason) = match resolve_agent(
        args.agent.as_deref(),
        &item_detail,
        &labels,
        &router_config,
        &installed,
    ) {
        Ok(pair) => pair,
        Err(msg) => {
            release_and_comment(&mcp, item_id, &msg, args.notify.as_deref());
            crate::ui::error(&msg);
            return 1.into();
        }
    };
    if headless_args(agent_enum).is_none() {
        let msg = format!("agent {} has no headless print mode", agent_enum.as_str());
        release_and_comment(&mcp, item_id, &msg, args.notify.as_deref());
        crate::ui::error(&msg);
        return 1.into();
    }
    let _ = writeln!(log, "agent: {} ({route_reason})", agent_enum.as_str());

    let prompt = build_prompt(&item_detail, &comments);

    // --- Extra args ---
    let extra_args = build_extra_args(agent_enum, args.max_turns, args.max_cost_usd);

    // --- Change to worktree dir and run ---
    let original_dir = std::env::current_dir().ok();
    if std::env::set_current_dir(wpath).is_err() {
        let msg = format!("failed to chdir into {}", wpath.display());
        release_and_comment(&mcp, item_id, &msg, args.notify.as_deref());
        crate::ui::error(&msg);
        return 1.into();
    }

    let outcome = agent_launch::run_headless(
        agent_registry::REGISTRY,
        agent_enum.as_str(),
        &prompt,
        timeout,
        idle_timeout,
        &extra_args,
    );

    // Restore cwd regardless of outcome.
    if let Some(d) = original_dir {
        let _ = std::env::set_current_dir(d);
    }

    // --- Report ---
    match outcome {
        HeadlessOutcome::Ok(reply) => {
            let (reply_text, session_id, cost_usd) =
                if agent_enum == agent_registry::Agent::ClaudeCode {
                    parse_claude_reply(&reply)
                } else {
                    (reply, None, None)
                };

            // The agent may already have called `done` itself with its own
            // `summary` (in which case this second call is a no-op — the
            // claim is already released) -- but the common case is a
            // headless run that just replies with text and lets this
            // wrapper handle `done`, so pass the parsed reply through as
            // the PR body rather than leaving it as the generic
            // placeholder.
            let done_resp = match mcp.item_done(ItemRequest {
                action: "done".into(),
                id: Some(item_id.into()),
                summary: Some(reply_text.clone()),
                ..Default::default()
            }) {
                Ok(j) => j,
                Err(e) => {
                    crate::ui::error(&format!("item_done failed: {}", e.message));
                    return 1.into();
                }
            };
            let done_val: serde_json::Value =
                serde_json::from_str(&done_resp).unwrap_or(serde_json::Value::Null);
            let pr_url = done_val["pr_url"].as_str().map(str::to_string);

            let comment_body = format_success_comment(
                &reply_text,
                session_id.as_deref(),
                cost_usd,
                pr_url.as_deref(),
            );
            let _ = mcp.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.into()),
                body: Some(comment_body.clone()),
                ..Default::default()
            });
            if let Some(recipient) = args.notify.as_deref() {
                notify(recipient, &comment_body, item_id);
            }

            let _ = writeln!(log, "done: {item_id}");
            if let Some(url) = &pr_url {
                let _ = writeln!(log, "pr: {url}");
            }
            0.into()
        }
        other => {
            let msg = failure_message(&other);
            release_and_comment(&mcp, item_id, &msg, args.notify.as_deref());
            crate::ui::error(&msg);
            let _ = writeln!(log, "failed: {msg}");
            let retry_after_secs = classify_and_cooldown(agent_enum.as_str(), &msg);
            WorkOutcome {
                exit_code: 1,
                retry_after_secs,
            }
        }
    }
}

/// Runs an in-process work-item dispatch job for `agentflare_jobs::WorkerPool`
/// (see `dispatch_item` in `src/supervisor.rs`, which enqueues jobs this
/// executes) instead of the daemon spawning a fresh `agentflare work`
/// subprocess per item. `args` is `[item_id, agent]` — see `dispatch_item`
/// for how it's built.
pub struct WorkItemExecutor;

impl agentflare_jobs::InProcessExecutor for WorkItemExecutor {
    fn execute(
        &self,
        job_id: &str,
        args: &[String],
        log: &mut dyn std::io::Write,
    ) -> Result<(), agentflare_jobs::JobFailure> {
        let (Some(item_id), Some(agent)) = (args.first(), args.get(1)) else {
            return Err(format!(
                "malformed in-process work job: expected [item_id, agent], got {args:?}"
            )
            .into());
        };
        let work_args = WorkArgs {
            target: item_id.clone(),
            agent: Some(agent.clone()),
            timeout: DEFAULT_TIMEOUT_SECS,
            idle_timeout: DEFAULT_IDLE_TIMEOUT_SECS,
            max_turns: None,
            max_cost_usd: None,
            notify: None,
        };
        // `<agent>:<job-id>` — the job's own queue id is a natural instance
        // discriminator, playing the role a subprocess's unique pid plays
        // for `claims::owner_id()` in the CLI path (see its doc comment).
        let owner = format!("{agent}:{job_id}");
        let outcome = crate::claims::with_owner_override(owner, || execute_work(work_args, log));
        if outcome.exit_code == 0 {
            Ok(())
        } else {
            Err(agentflare_jobs::JobFailure {
                message: format!(
                    "agentflare work exited with code {} — see the job log for details",
                    outcome.exit_code
                ),
                retry_after_secs: outcome.retry_after_secs,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_item() -> agentflare_backend::item::Item {
        agentflare_backend::item::Item {
            id: "item-1".into(),
            project_id: "proj-1".into(),
            state_id: "state-1".into(),
            name: "Fix the flaky test".into(),
            description: "test_foo fails ~1 in 20 runs".into(),
            priority: "medium".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id: 42,
            sort_order: 0.0,
            started_at: None,
            completed_at: None,
            archived_at: None,
            external_source: None,
            external_id: None,
            metadata: "{}".into(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
        }
    }

    #[test]
    fn build_prompt_includes_name_description_and_comments() {
        let item = test_item();
        let comments = vec![agentflare_backend::comment::ItemComment {
            id: "c1".into(),
            item_id: "item-1".into(),
            author_agent: "alice".into(),
            body: "probably a race in the setup fixture".into(),
            created_at: 0,
            updated_at: 0,
        }];
        let prompt = build_prompt(&item, &comments);
        assert!(prompt.contains("#42"));
        assert!(prompt.contains("Fix the flaky test"));
        assert!(prompt.contains("test_foo fails ~1 in 20 runs"));
        assert!(prompt.contains("alice"));
        assert!(prompt.contains("probably a race"));
    }

    #[test]
    fn build_prompt_omits_discussion_section_when_no_comments() {
        let item = test_item();
        let prompt = build_prompt(&item, &[]);
        assert!(!prompt.contains("Prior discussion"));
    }

    #[test]
    fn parse_claude_reply_extracts_structured_fields() {
        let raw = r#"{"result":"Fixed the race by adding a mutex.","session_id":"sess-123","total_cost_usd":0.0842}"#;
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, "Fixed the race by adding a mutex.");
        assert_eq!(session_id.as_deref(), Some("sess-123"));
        assert_eq!(cost, Some(0.0842));
    }

    #[test]
    fn parse_claude_reply_extracts_the_result_from_the_last_line_of_a_stream_json_transcript() {
        // --output-format stream-json emits one JSON object per line (system
        // init, tool_use/tool_result, assistant messages, ...) and only the
        // FINAL line carries the same {"result":...} shape the single-object
        // `json` format uses — everything before it must be ignored, not
        // treated as (or blended into) the reply text.
        let raw = concat!(
            r#"{"type":"system","subtype":"init","session_id":"sess-123"}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"working..."}]}}"#,
            "\n",
            r#"{"type":"result","result":"Fixed the race by adding a mutex.","session_id":"sess-123","total_cost_usd":0.0842}"#,
        );
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, "Fixed the race by adding a mutex.");
        assert_eq!(session_id.as_deref(), Some("sess-123"));
        assert_eq!(cost, Some(0.0842));
    }

    #[test]
    fn parse_claude_reply_falls_back_to_raw_text_on_non_json() {
        let raw = "plain text reply, no JSON here";
        let (text, session_id, cost) = parse_claude_reply(raw);
        assert_eq!(text, raw);
        assert!(session_id.is_none());
        assert!(cost.is_none());
    }

    #[test]
    fn resolve_agent_explicit_flag_wins_even_over_item_assignment() {
        let mut item = test_item();
        item.assignee_agent = Some("opencode".to_string());
        let config = agent_registry::RouterConfig::default();
        let (agent, reason) = resolve_agent(Some("codex"), &item, &[], &config, &[]).unwrap();
        assert_eq!(agent, agent_registry::Agent::Codex);
        assert_eq!(reason, "explicit --agent flag");
    }

    #[test]
    fn resolve_agent_explicit_flag_accepts_an_alias() {
        // "claude" isn't a registry id, canonicalize() maps it to claude-code
        // — the explicit path must accept it the same as assignee_agent does.
        let item = test_item();
        let config = agent_registry::RouterConfig::default();
        let (agent, _) = resolve_agent(Some("claude"), &item, &[], &config, &[]).unwrap();
        assert_eq!(agent, agent_registry::Agent::ClaudeCode);
    }

    #[test]
    fn resolve_agent_explicit_flag_rejects_unknown_agent_name() {
        let item = test_item();
        let config = agent_registry::RouterConfig::default();
        let err = resolve_agent(Some("not-a-real-agent"), &item, &[], &config, &[]).unwrap_err();
        assert!(err.contains("unknown agent"));
    }

    #[test]
    fn resolve_agent_falls_back_to_the_items_own_assignee() {
        let mut item = test_item();
        item.assignee_agent = Some("claude".to_string()); // alias, agent_by_name() maps it
        let config = agent_registry::RouterConfig::default();
        let (agent, reason) = resolve_agent(None, &item, &[], &config, &[]).unwrap();
        assert_eq!(agent, agent_registry::Agent::ClaudeCode);
        assert_eq!(reason, "explicit assignment on task");
    }

    #[test]
    fn resolve_agent_falls_back_to_an_instance_suffixed_assignee() {
        // A previously-claimed item's assignee_agent carries
        // `<agent>:<instance>` (see item::claim's doc comment) — this must
        // still route correctly, or a once-claimed item silently loses its
        // assignee on the next auto-routed dispatch.
        let mut item = test_item();
        item.assignee_agent = Some("claude-code:some-job-id".to_string());
        let config = agent_registry::RouterConfig::default();
        let (agent, reason) = resolve_agent(None, &item, &[], &config, &[]).unwrap();
        assert_eq!(agent, agent_registry::Agent::ClaudeCode);
        assert_eq!(reason, "explicit assignment on task");
    }

    #[test]
    fn resolve_agent_errors_when_no_flag_and_no_assignment_and_no_rule() {
        let item = test_item();
        let config = agent_registry::RouterConfig::default();
        let err = resolve_agent(
            None,
            &item,
            &[],
            &config,
            &[agent_registry::Agent::ClaudeCode],
        )
        .unwrap_err();
        assert!(err.contains("--agent"));
    }

    #[test]
    fn resolve_agent_uses_a_configured_rule_when_the_item_has_no_assignee() {
        let mut item = test_item();
        item.metadata = r#"{"size":"S"}"#.to_string();
        let config = agent_registry::parse_router_config(
            r#"
[router]
[[router.rule]]
when = { size = "S" }
use  = "opencode"
"#,
        )
        .unwrap();
        let (agent, _) = resolve_agent(
            None,
            &item,
            &[],
            &config,
            &[agent_registry::Agent::Opencode],
        )
        .unwrap();
        assert_eq!(agent, agent_registry::Agent::Opencode);
    }

    #[test]
    fn resolve_agent_matches_a_rule_on_the_items_kind() {
        let mut item = test_item();
        item.metadata = r#"{"kind":"locate"}"#.to_string();
        let config = agent_registry::parse_router_config(
            r#"
[router]
[[router.rule]]
when = { kind = "locate" }
use  = "opencode"
"#,
        )
        .unwrap();
        let (agent, _) = resolve_agent(
            None,
            &item,
            &[],
            &config,
            &[agent_registry::Agent::Opencode],
        )
        .unwrap();
        assert_eq!(agent, agent_registry::Agent::Opencode);
    }

    #[test]
    fn format_success_comment_includes_pr_and_metadata() {
        let body = format_success_comment(
            "Fixed the race.",
            Some("sess-123"),
            Some(0.08),
            Some("https://github.com/o/r/pull/9"),
        );
        assert!(body.contains("Fixed the race."));
        assert!(body.contains("https://github.com/o/r/pull/9"));
        assert!(body.contains("sess-123"));
        assert!(body.contains("0.08"));
    }

    #[test]
    fn format_success_comment_omits_metadata_block_when_absent() {
        let body = format_success_comment("Fixed the race.", None, None, None);
        assert!(!body.contains("session:"));
        assert!(!body.contains("cost:"));
    }

    #[test]
    fn failure_message_extracts_inner_string() {
        let outcome = HeadlessOutcome::NotFound("claude not found".into());
        assert_eq!(failure_message(&outcome), "claude not found");
    }

    #[test]
    fn failure_message_includes_diagnostic_suffix_for_plain_failures() {
        let outcome = HeadlessOutcome::Failed(format!(
            "claude-code exited non-zero — last stderr before kill:\n{}",
            "HTTP 429 Too Many Requests"
        ));
        let msg = failure_message(&outcome);
        assert!(msg.contains("HTTP 429 Too Many Requests"));
    }

    #[test]
    fn classify_and_cooldown_ignores_non_rate_limit_failures() {
        crate::paths::test_support::with_temp_home(|| {
            let retry = classify_and_cooldown("claude-code", "something went wrong");
            assert!(retry.is_none());
            let conn = crate::auth_db::open_or_rebuild();
            assert!(!crate::auth_db::is_cooling_down(&conn, "claude-code"));
        });
    }

    #[test]
    fn classify_and_cooldown_sets_a_cooldown_on_rate_limit_shaped_failures() {
        crate::paths::test_support::with_temp_home(|| {
            let retry = classify_and_cooldown("claude-code", "HTTP 429 Too Many Requests");
            assert_eq!(retry, Some(RATE_LIMIT_COOLDOWN_MINUTES as u64 * 60));
            let conn = crate::auth_db::open_or_rebuild();
            assert!(crate::auth_db::is_cooling_down(&conn, "claude-code"));
        });
    }

    #[test]
    fn build_extra_args_includes_bypass_and_streaming_output_for_claude() {
        // Plain `--output-format json` writes NOTHING to stdout/stderr until
        // the entire run finishes (confirmed by hand: 0 bytes for 54s+ on a
        // trivial 2-tool-call task) — run_captured's idle-timeout (default
        // 300s) then kills any real task before it can finish, every time.
        // `stream-json` (+ the `--verbose` it requires) emits one JSON
        // object per turn/tool-call as it happens, giving a genuine
        // liveness signal; its final line carries the same {"result":...}
        // shape `parse_claude_reply` already expects.
        let args = build_extra_args(agent_registry::Agent::ClaudeCode, None, None);
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
        assert!(!args.contains(&"json".to_string()));
        assert!(args.contains(&"--verbose".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("--max-turns")));
    }

    #[test]
    fn build_extra_args_passes_through_max_turns_and_cost_for_claude() {
        let args = build_extra_args(agent_registry::Agent::ClaudeCode, Some(5), Some(2.5));
        assert!(args.contains(&"--max-turns=5".to_string()));
        assert!(args.contains(&"--max-budget-usd=2.5".to_string()));
    }

    #[test]
    fn build_extra_args_for_codex_has_bypass_but_no_json_output() {
        let args = build_extra_args(agent_registry::Agent::Codex, None, None);
        assert_eq!(args, vec!["--full-auto".to_string()]);
    }

    fn seeded_item(
        mcp: &AgentflareMcp,
        conn: &rusqlite::Connection,
    ) -> agentflare_backend::item::Item {
        let project = mcp.resolve_project(conn).unwrap();
        let state = agentflare_backend::state::list_by_project(conn, &project.id)
            .unwrap()
            .into_iter()
            .find(|s| s.is_default)
            .unwrap();
        agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id,
                state_id: state.id,
                name: "Integration test item".into(),
                description: Some("do the thing".into()),
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap()
    }

    fn init_test_repo(root: &std::path::Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
        };
        run(&["init", "-b", "master"]);
        run(&["config", "user.email", "test@test.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["commit", "--allow-empty", "-m", "initial"]);
    }

    #[test]
    fn claim_then_headless_not_found_releases_claim_and_posts_error_comment() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        init_test_repo(&repo_root);
        let backend_db = tmp.path().join("backend.db");
        let project_link = tmp.path().join("project.json");

        let mcp = AgentflareMcp::for_test(backend_db.clone(), repo_root.clone(), project_link);
        let item = mcp.with_backend_db(|conn| seeded_item(&mcp, conn)).unwrap();

        let claim_json = mcp
            .item_claim(ItemRequest {
                action: "claim".to_string(),
                id: Some(item.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let claim: serde_json::Value = serde_json::from_str(&claim_json).unwrap();
        assert_eq!(claim["status"], "acquired");
        assert!(claim["worktree_path"].as_str().is_some());

        // Simulate the failure branch `run_work` takes when the agent binary
        // isn't on PATH — release + comment, the same helper `run_work` calls.
        release_and_comment(&mcp, &item.id, "claude-code not found on PATH", None);

        let claim_after = mcp
            .item_claim(ItemRequest {
                action: "claim".to_string(),
                id: Some(item.id.clone()),
                ..Default::default()
            })
            .unwrap();
        let claim_after: serde_json::Value = serde_json::from_str(&claim_after).unwrap();
        assert_eq!(
            claim_after["status"], "acquired",
            "claim must be released so a re-claim succeeds"
        );

        let comments = mcp
            .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
            .unwrap()
            .unwrap();
        assert_eq!(comments.len(), 1);
        assert!(comments[0].body.contains("claude-code not found on PATH"));
    }

    #[test]
    fn claiming_an_already_held_item_reports_held_without_side_effects() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        init_test_repo(&repo_root);
        let backend_db = tmp.path().join("backend.db");
        let project_link = tmp.path().join("project.json");

        let mcp = AgentflareMcp::for_test(backend_db.clone(), repo_root.clone(), project_link);
        let item = mcp.with_backend_db(|conn| seeded_item(&mcp, conn)).unwrap();

        let first = mcp
            .item_claim(ItemRequest {
                action: "claim".to_string(),
                id: Some(item.id.clone()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&first).unwrap()["status"],
            "acquired"
        );

        // A second claim by a different owner is what a real second worker
        // process would see — proven directly at the DB/ledger level, since
        // this test process's own identity is the same for both calls above.
        let held = mcp
            .with_backend_db(|conn| {
                agentflare_backend::claim::has_active_claim_by_other(
                    conn,
                    &item.id,
                    "someone-else:1",
                    crate::claims::now(),
                    crate::mcp_server::types::backend_claim_ttl_secs(),
                )
            })
            .unwrap()
            .unwrap();
        assert!(held, "item must show as actively claimed by another owner");
    }
}
