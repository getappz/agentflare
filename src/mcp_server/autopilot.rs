//! `autopilot` MCP tool: backing policy for the Claude plugin mod.

use super::*;

#[derive(Debug, Default, serde::Deserialize)]
pub struct CheckpointItem {
    #[serde(default)]
    pub id: String,
    pub seq: Option<i64>,
    #[serde(default)]
    pub title: String,
}

/// Session state the mod hands over before compaction.
#[derive(Debug, Default, serde::Deserialize)]
pub struct CheckpointPayload {
    pub item: Option<CheckpointItem>,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    /// Absent = derived from the session record (see `fill_gate_freshness`).
    pub verification_fresh: Option<bool>,
    pub review_fresh: Option<bool>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub remaining: Vec<String>,
}

/// Re-inject text for a checkpoint; pure so it works with or without memory.
pub fn checkpoint_text(p: &CheckpointPayload) -> String {
    let mut t = String::from("[agentflare checkpoint]");
    if let Some(i) = &p.item {
        let n = i.seq.map_or_else(|| i.id.clone(), |s| format!("#{s}"));
        t.push_str(&format!(
            "
item: {n} {}",
            i.title
        ));
    }
    if let Some(b) = &p.branch {
        t.push_str(&format!(
            "
branch: {b}"
        ));
    }
    if let Some(w) = &p.worktree {
        t.push_str(&format!(
            "
worktree: {w}"
        ));
    }
    t.push_str(&format!(
        "
gates: tests {}, review {}",
        if p.verification_fresh.unwrap_or(false) {
            "fresh"
        } else {
            "stale"
        },
        if p.review_fresh.unwrap_or(false) {
            "fresh"
        } else {
            "stale"
        }
    ));
    for (label, list) in [("decisions", &p.decisions), ("remaining", &p.remaining)] {
        if !list.is_empty() {
            t.push_str(&format!(
                "
{label}:"
            ));
            for x in list {
                t.push_str(&format!(
                    "
- {x}"
                ));
            }
        }
    }
    t
}

impl AgentflareMcp {
    pub fn autopilot_impl(&self, req: AutopilotRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "health" => Ok(Self::autopilot_health().to_string()),
            "check" => Ok(Self::autopilot_check(&req).to_string()),
            "observe" => Ok(Self::autopilot_observe(&req).to_string()),
            "briefing" => Ok(self.autopilot_briefing(&req).to_string()),
            "checkpoint" => Self::autopilot_checkpoint(&req).map(|v| v.to_string()),
            "next_work" => self.autopilot_next_work(&req).map(|v| v.to_string()),
            other => Err(ErrorData::invalid_params(
                format!(
                    "unknown autopilot action `{other}` (expected check|observe|briefing|checkpoint|next_work|health)"
                ),
                None,
            )),
        }
    }

    fn autopilot_health() -> serde_json::Value {
        let daemon = match crate::daemon::read_pid_from_file() {
            None => "down",
            Some(pid) => {
                let addr = crate::ipc::DaemonAddr::default_for_pid(pid);
                match crate::daemon_client::daemon_health_check(&addr) {
                    Ok(_) => "up",
                    Err(_) => "unknown",
                }
            }
        };
        let stale_binary = std::env::current_exe()
            .map(|exe| !crate::doctor::stale_hook_binaries(&exe, Some("claude")).is_empty())
            .unwrap_or(false);
        // lean-ctx: auth cooldown state not probed yet, upgrade when `next_work` needs it
        serde_json::json!({
            "daemon": daemon,
            "stale_binary": stale_binary,
            "auth": "ok",
            "mcp_version": env!("CARGO_PKG_VERSION"),
        })
    }

    /// Records verification/review/diagnosis evidence through the same
    /// function the PostToolUse hook uses. The tree is fingerprinted from the
    /// request's `cwd` (see `check`), so both actions compare the same tree.
    fn autopilot_observe(req: &AutopilotRequest) -> serde_json::Value {
        let raw_name = req.tool_name.as_deref().unwrap_or_default();
        let (tool, input) =
            crate::hook_redirect::unwrap_gateway_call(raw_name, req.tool_input.as_ref());
        let command = input.as_ref().and_then(|ti| {
            ["command", "cmd", "script", "CommandLine"]
                .into_iter()
                .find_map(|k| ti.get(k).and_then(|v| v.as_str()))
        });
        let cwd = req
            .cwd
            .clone()
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|d| d.display().to_string())
            })
            .unwrap_or_default();
        let obs = crate::hook_completion_gate::Observation {
            session_id: req.session_id.as_deref().unwrap_or_default(),
            cwd: &cwd,
            agent: req.agent.as_deref().unwrap_or("claude"),
            tool_name: &tool,
            command,
            exit_code: req.exit_code,
            output_text: req.output_excerpt.as_deref().unwrap_or_default(),
            review_submitted: tool == "mcp__flare__review"
                && req.item_action.as_deref() == Some("submit")
                && req.item_success == Some(true),
            trust_end_tree: true,
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut runtime = crate::optimize::load_runtime();
        let recorded = crate::hook_completion_gate::record_observation(&mut runtime, &obs, now);
        if recorded != crate::hook_completion_gate::Recorded::None {
            crate::optimize::save_runtime(&runtime);
        }
        serde_json::json!({ "recorded": recorded.as_str() })
    }

    /// Guard parity with the PreToolUse hook: same functions, same order,
    /// same reason strings. `cwd` is applied by absolutizing the tool's path
    /// (never `set_current_dir`).
    fn autopilot_check(req: &AutopilotRequest) -> serde_json::Value {
        use crate::hook_redirect::ITEM_TOOL_NAME;
        let raw_name = req.tool_name.as_deref().unwrap_or_default();
        let (tool, input) =
            crate::hook_redirect::unwrap_gateway_call(raw_name, req.tool_input.as_ref());
        let is_item = tool == ITEM_TOOL_NAME || tool == "item";
        // No early allow: like the hook, every tool runs the redirect, claim and
        // coaching checks (the data guard, shell git-write targets, TodoWrite);
        // only mutating tools make them spawn git.
        let input = Self::anchor_to_cwd(&tool, input, req.cwd.as_deref());
        let deny = |rule: &str, reason: String| serde_json::json!({ "decision": "deny", "rule": rule, "reason": reason });
        let hook_reason = |d: &serde_json::Value| {
            d["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
        if let Some(d) = crate::hook_redirect::redirect_decision(&tool, input.as_ref()) {
            let reason = hook_reason(&d);
            let rule = if reason.contains("local data files") {
                "data_guard"
            } else if reason.contains("default branch") {
                "branch_guard"
            } else {
                "redirect"
            };
            return deny(rule, reason);
        }
        if let Some(d) = crate::hook_claim_guard::claim_guard_decision(&tool, input.as_ref()) {
            return deny("claim_guard", hook_reason(&d));
        }
        if let Some(reason) = crate::coaching::enforced_rule_reason_for_tool(&tool, input.as_ref())
        {
            return deny("coaching", reason);
        }
        if is_item {
            let agent = req.agent.as_deref().unwrap_or("claude");
            let key = crate::optimize::scoped_session_key(
                agent,
                req.session_id.as_deref().unwrap_or_default(),
            );
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // Missing record = no fresh evidence, same as the hook's fresh entry.
            let record = crate::optimize::load_runtime()
                .sessions
                .remove(&key)
                .unwrap_or(crate::optimize::SessionRecord {
                    start_ts: now,
                    turn_count: 0,
                    recent_tool_calls: vec![],
                    last_verification: None,
                    last_review: None,
                    last_diagnosis: None,
                    verification_start_tree: None,
                });
            let task_type = crate::hook::resolve_item_task_type(&tool, input.as_ref());
            // Same explicit cwd `observe` fingerprints, never the MCP process cwd.
            let cwd = req
                .cwd
                .as_deref()
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::current_dir().ok());
            if let Some(reason) = crate::hook_completion_gate::completion_gate_denial_in(
                &record,
                now,
                &tool,
                input.as_ref(),
                task_type.as_deref(),
                cwd.as_deref(),
            ) {
                return deny("completion_gate", reason);
            }
        }
        serde_json::json!({ "decision": "allow" })
    }

    /// Session briefing text with a content etag; `unchanged` when the caller's
    /// etag still matches so the mod can skip re-injecting it.
    fn autopilot_briefing(&self, req: &AutopilotRequest) -> serde_json::Value {
        use sha2::{Digest, Sha256};
        let agent = req.agent.as_deref().unwrap_or("claude");
        let cwd = req.cwd.as_deref().map(std::path::Path::new);
        let recall = Self::briefing_recall(cwd);
        let db_path = self
            .backend_db_override
            .clone()
            .unwrap_or_else(|| crate::paths::agentflare_dir().join("backend.db"));
        let text = crate::hook::briefing_text_in(agent, cwd, recall.as_deref(), &db_path);
        let etag = Sha256::digest(text.as_bytes())
            .iter()
            .fold(String::new(), |mut s, b| {
                s.push_str(&format!("{b:02x}"));
                s
            });
        if req.etag.as_deref() == Some(etag.as_str()) {
            return serde_json::json!({ "etag": etag, "unchanged": true });
        }
        serde_json::json!({ "etag": etag, "text": text })
    }

    /// Request: `session_id`, `payload`, optional `cwd` (tree for gate
    /// freshness) and `agent` (default `claude`; scopes the session record).
    /// Response: `{ handoff_id, text }`. Stores a memory handoff and returns
    /// the re-inject text. A failed
    /// memory write yields `handoff_id: null` -- never an error, since the mod
    /// must always get a checkpoint to re-inject. The id is the session id
    /// (handoffs are keyed by it).
    fn autopilot_checkpoint(req: &AutopilotRequest) -> Result<serde_json::Value, ErrorData> {
        let bad = |m: &str| ErrorData::invalid_params(m.to_string(), None);
        let sid = req
            .session_id
            .clone()
            .ok_or_else(|| bad("session_id is required"))?;
        let mut payload: CheckpointPayload = serde_json::from_value(
            req.payload
                .clone()
                .ok_or_else(|| bad("payload is required"))?,
        )
        .map_err(|e| bad(&format!("invalid payload: {e}")))?;
        Self::fill_gate_freshness(req, &sid, &mut payload);
        let text = checkpoint_text(&payload);
        let strs = |v: &[String]| Some(v.iter().map(|s| serde_json::json!(s)).collect());
        let stored = crate::memory::mcp::handle_handoff(crate::memory::mcp::HandoffInput {
            session_id: sid.clone(),
            summary: text.clone(),
            findings: None,
            decisions: strs(&payload.decisions),
            files_touched: None,
            evidence: None,
            completed: None,
            remaining: strs(&payload.remaining),
            source_ref: Some(format!("claude_code:{sid}")),
            dropped_fields: None,
        });
        let handoff_id = stored.ok().map(|_| sid);
        Ok(serde_json::json!({ "handoff_id": handoff_id, "text": text }))
    }

    /// Fills `verification_fresh`/`review_fresh` the payload left out from the
    /// session record, with the completion gate's own freshness functions over
    /// the request `cwd` tree (a missing record counts as stale). Flags the
    /// caller did send are kept as they are.
    fn fill_gate_freshness(req: &AutopilotRequest, sid: &str, p: &mut CheckpointPayload) {
        if p.verification_fresh.is_some() && p.review_fresh.is_some() {
            return;
        }
        let key =
            crate::optimize::scoped_session_key(req.agent.as_deref().unwrap_or("claude"), sid);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let record = crate::optimize::load_runtime().sessions.remove(&key);
        let tree = req
            .cwd
            .as_deref()
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .and_then(|d| crate::optimize::tree_fingerprint_in(&d));
        let fresh = |f: fn(&crate::optimize::SessionRecord, u64, Option<&str>) -> bool| {
            record.as_ref().is_some_and(|r| f(r, now, tree.as_deref()))
        };
        p.verification_fresh
            .get_or_insert_with(|| fresh(crate::optimize::has_fresh_passing_verification));
        p.review_fresh
            .get_or_insert_with(|| fresh(crate::optimize::has_fresh_review));
    }

    /// Memory recall for "<repo> <cwd basename>"; any failure or empty result
    /// is silently omitted -- the briefing must never fail because memory is down.
    fn briefing_recall(cwd: Option<&std::path::Path>) -> Option<String> {
        let base = cwd?.file_name()?.to_string_lossy().into_owned();
        let query = match crate::claims::resolve_repo(None) {
            Some(repo) => format!("{repo} {base}"),
            None => base,
        };
        crate::memory::mcp::handle_recall(crate::memory::mcp::RecallInput {
            query: Some(query),
            id: None,
            r#type: None,
            project: None,
            limit: Some(5),
        })
        .ok()
        .filter(|r| !matches!(r.trim(), "" | "[]"))
    }

    /// Resolve a relative target path against `cwd`; a mutating tool with no
    /// path targets `cwd` itself (the hook falls back to process cwd).
    fn anchor_to_cwd(
        tool: &str,
        input: Option<serde_json::Value>,
        cwd: Option<&str>,
    ) -> Option<serde_json::Value> {
        let (Some(cwd), Some(mut v)) = (cwd, input.clone()) else {
            return input;
        };
        let Some(obj) = v.as_object_mut() else {
            return input;
        };
        let key = ["file_path", "path", "filePath"]
            .into_iter()
            .find(|k| obj.get(*k).is_some_and(|x| x.is_string()));
        match key {
            Some(k) => {
                let p = obj[k].as_str().unwrap_or_default();
                if !std::path::Path::new(p).is_absolute() {
                    let joined = std::path::Path::new(cwd).join(p);
                    obj.insert(k.into(), joined.display().to_string().into());
                }
            }
            None if crate::hook_redirect::MUTATING_TOOLS.contains(&tool) => {
                // `.` makes the ancestor walk start at cwd itself.
                let here = std::path::Path::new(cwd).join(".");
                obj.insert("path".into(), here.display().to_string().into());
            }
            None => {}
        }
        Some(v)
    }
}
