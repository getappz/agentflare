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
                "actions": flare_browser::ACTIONS.iter().map(|a| a.name).collect::<Vec<_>>(),
            });
            return Ok(serde_json::to_string_pretty(&result).unwrap_or_default());
        }

        // Positional assembly mirrors the CLI: target, then text, then url —
        // matches sidecar order for fill <sel> <text>, open <url>, eval <js>.
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
        let extra = req.args.unwrap_or_default();

        // `observe` composes locally: snapshot, then filter (no model call).
        // The query is consumed here, not forwarded as a sidecar positional —
        // `snapshot` takes no arguments.
        let (backend_action, observe_query) = if action == "observe" {
            let q = if positionals.is_empty() {
                String::new()
            } else {
                positionals.remove(0)
            };
            if q.trim().is_empty() {
                return Err(ErrorData::invalid_params(
                    "observe requires a query in `text`",
                    None,
                ));
            }
            ("snapshot".to_string(), Some(q))
        } else {
            (action.clone(), None)
        };

        let output = tokio::task::spawn_blocking({
            let session = session.clone();
            let auto_install = flare_browser::auto_install_enabled();
            move || -> Result<String, String> {
                let backend = crate::browser_install::ensure_agent_browser(auto_install)?;
                let argv =
                    flare_browser::build_argv(&session, &backend_action, &positionals, &extra)?;
                flare_browser::run_blocking(&backend, &argv)
            }
        })
        .await
        .map_err(|e| ErrorData::internal_error(format!("browser task join: {e}"), None))?
        .map_err(|e: String| ErrorData::internal_error(e, None))?;

        let mut output = flare_browser::compact_output(&output, flare_browser::MAX_OUTPUT_CHARS);
        if let Some(q) = observe_query {
            output = flare_browser::observe_filter(&output, &q, 40);
        }
        output = flare_browser::redact(&output, &secrets);
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
}
