//! `browser` MCP tool — single consolidated action-dispatch frontend over
//! the `flare-browser` crate (same argv/session/redaction core as the
//! `agentflare browser` CLI, so both surfaces stay in lockstep).
//!
//! Subprocess execution runs inside `spawn_blocking`: `tokio` here has no
//! `process` feature, and a blocking sidecar call must never park an async
//! worker thread.

use super::*;

impl AgentflareMcp {
    pub(crate) async fn browser_impl(&self, req: BrowserRequest) -> Result<String, ErrorData> {
        let action = req.action.trim().to_string();
        if action.is_empty() {
            return Err(ErrorData::invalid_params("action is required", None));
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let session = flare_browser::resolve_session(req.session.as_deref(), &cwd);
        let secrets = req.redact.unwrap_or_default();

        // Local-only: report backend presence + session without launching Chrome.
        if action == "status" {
            let (backend, path) = match flare_browser::find_backend() {
                Ok(p) => (true, Some(p.display().to_string())),
                Err(_) => (false, None),
            };
            let result = serde_json::json!({
                "action": "status",
                "session": session,
                "backend_present": backend,
                "backend_path": path,
                "actions": flare_browser::ACTIONS.iter().map(|a| a.name).chain(["plan", "act"]).collect::<Vec<_>>(),
            });
            return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
        }

        if action == "plan" || action == "act" {
            if (action == "act"
                && (req.target.is_some() || req.text.is_some() || req.operation.is_some()))
                || (action == "plan" && req.decision.is_some())
            {
                return Err(ErrorData::invalid_params(
                    "act accepts a decision id; plan accepts an operation and observed target",
                    None,
                ));
            }
            if req.url.is_some() || req.args.as_ref().is_some_and(|a| !a.is_empty()) {
                return Err(ErrorData::invalid_params(
                    "plan/act do not accept URL or extra args",
                    None,
                ));
            }
            let mut args = req.target.clone().into_iter().collect::<Vec<_>>();
            args.extend(req.text.clone());
            let operation = req.operation.unwrap_or_default();
            if action == "plan" {
                crate::browser_decision::validate(&operation, &args)
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
            }
            let token = req.decision.unwrap_or_default();
            let planning = action == "plan";
            let session_for_task = session.clone();
            let output = tokio::task::spawn_blocking(move || {
                let dir = crate::paths::agentflare_dir().join("browser-decisions");
                let runner = |operation: &str, args: &[String]| {
                    let argv = flare_browser::build_argv(&session_for_task, operation, args, &[])?;
                    let (backend, path_env) = crate::browser_install::ensure_agent_browser(
                        flare_browser::auto_install_enabled(),
                    )?;
                    // Fingerprint raw state: redaction must not hide a page change.
                    flare_browser::run_blocking(&backend, &argv, &[], path_env.as_deref())
                };
                if planning {
                    crate::browser_decision::plan_with(
                        &dir,
                        &session_for_task,
                        &operation,
                        &args,
                        runner,
                    )
                } else {
                    crate::browser_decision::act_with(&dir, &session_for_task, &token, runner)
                }
            })
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
            .map_err(|e| ErrorData::internal_error(flare_browser::redact(&e, &secrets), None))?;
            return Ok(serde_json::json!({"action":action,"session":session,"read_only":planning,"output":flare_browser::compact_output(&flare_browser::redact(&output, &secrets), flare_browser::MAX_OUTPUT_CHARS)}).to_string());
        }

        // `observe` uses `text` directly as the local filter query and never
        // forwards positionals to the backend -- `snapshot` takes none, and
        // `target`/`url` must never silently substitute for a missing query.
        // Every other action assembles positionals mirroring the CLI:
        // target, then text, then url (fill <sel> <text>, open <url>, eval
        // <js>).
        let (backend_action, observe_query, positionals) = if action == "observe" {
            let q = req.text.unwrap_or_default();
            if q.trim().is_empty() {
                return Err(ErrorData::invalid_params(
                    "observe requires a query in `text`",
                    None,
                ));
            }
            ("snapshot".to_string(), Some(q), Vec::new())
        } else {
            let mut positionals = Vec::new();
            if let Some(t) = req.target.filter(|s| !s.trim().is_empty()) {
                positionals.push(t);
            }
            if let Some(t) = req.text.filter(|s| !s.trim().is_empty()) {
                positionals.push(t);
            }
            if let Some(u) = req.url.filter(|s| !s.trim().is_empty()) {
                positionals.push(u);
            }
            (action.clone(), None, positionals)
        };
        let extra = req.args.unwrap_or_default();

        // Validate before touching install/spawn: an unknown action should
        // fail as invalid_params, not trigger a sidecar install or surface
        // as an opaque backend error.
        let argv = flare_browser::build_argv(&session, &backend_action, &positionals, &extra)
            .map_err(|e| ErrorData::invalid_params(e, None))?;

        let output = tokio::task::spawn_blocking({
            let secrets = secrets.clone();
            let auto_install = flare_browser::auto_install_enabled();
            move || -> Result<String, String> {
                let (backend, path_env) =
                    crate::browser_install::ensure_agent_browser(auto_install)?;
                flare_browser::run_blocking(&backend, &argv, &secrets, path_env.as_deref())
            }
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("browser task join: {e}"), None))?
        .map_err(|e: String| {
            ErrorData::internal_error(flare_browser::redact(&e, &secrets), None)
        })?;

        // Filter before redacting (so a query matching a since-redacted
        // value still works) and redact before truncating (so a secret
        // never leaves a partial, unredacted prefix in a capped response).
        let mut output = if let Some(q) = observe_query {
            flare_browser::observe_filter(&output, &q, 40)
        } else {
            output
        };
        output = flare_browser::redact(&output, &secrets);
        output = flare_browser::compact_output(&output, flare_browser::MAX_OUTPUT_CHARS);
        let result = serde_json::json!({
            "action": action,
            "session": session,
            "read_only": flare_browser::is_read_only(&action),
            "output": output,
        });
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn status_reports_session_without_launching_a_browser() {
        let mcp = AgentflareMcp::default();
        let req = BrowserRequest {
            action: "status".to_string(),
            ..Default::default()
        };
        let out = mcp.browser_impl(req).await.expect("status is local-only");
        assert!(out.contains("\"action\": \"status\""), "{out}");
        assert!(out.contains("\"session\":"), "{out}");
    }

    #[tokio::test]
    async fn empty_action_is_rejected_before_backend_lookup() {
        let mcp = AgentflareMcp::default();
        let req = BrowserRequest {
            action: "   ".to_string(),
            ..Default::default()
        };
        let err = mcp.browser_impl(req).await.unwrap_err();
        assert!(err.to_string().contains("action is required"), "{err}");
    }

    #[tokio::test]
    async fn research_patterns_browser_mcp_rejects_invalid_plan_before_backend() {
        let mcp = AgentflareMcp::default();
        let req = BrowserRequest {
            action: "plan".into(),
            operation: Some("eval".into()),
            target: Some("@e1".into()),
            ..Default::default()
        };
        assert!(
            mcp.browser_impl(req)
                .await
                .unwrap_err()
                .to_string()
                .contains("plan requires")
        );
    }
}
