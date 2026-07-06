//! MCP (Model Context Protocol) server over stdio, built on the `rmcp` crate
//! (`modelcontextprotocol/rust-sdk`, published to crates.io — a normal
//! dependency, not ported code; no /NOTICE entry needed).

use crate::optimize;
use crate::optimize::Router;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        AnnotateAble, ErrorData, Implementation, ListResourcesResult, PaginatedRequestParams,
        RawResource, ReadResourceRequestParams, ReadResourceResult, ResourceContents,
        ServerCapabilities, ServerInfo,
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

#[derive(Clone)]
pub struct AgentflareMcp {
    tool_router: ToolRouter<Self>,
}

impl AgentflareMcp {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
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
        let router = optimize::KeywordRouter;
        let result = match router.route(&ctx) {
            Some(nudge) => serde_json::json!({"suggestion": nudge}),
            None => serde_json::json!({"suggestion": null}),
        };
        serde_json::to_string_pretty(&result).unwrap_or_default()
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
}

pub async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service = AgentflareMcp::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_info_reports_agentflare_identity() {
        let s = AgentflareMcp::new();
        let info = s.get_info();
        assert_eq!(info.server_info.name, env!("CARGO_PKG_NAME"));
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn routing_suggestion_returns_null_for_non_locate() {
        let s = AgentflareMcp::new();
        let result = s.get_routing_suggestion(Parameters(GetRoutingSuggestionRequest {
            prompt: "refactor the payment module".to_string(),
        }));
        assert!(result.contains("null"));
    }

    #[test]
    fn routing_suggestion_returns_nudge_for_find() {
        let s = AgentflareMcp::new();
        let result = s.get_routing_suggestion(Parameters(GetRoutingSuggestionRequest {
            prompt: "find the auth handler".to_string(),
        }));
        assert!(result.contains("cheap-model"));
    }

    #[test]
    fn check_session_health_unknown_returns_status() {
        let s = AgentflareMcp::new();
        let result = s
            .check_session_health(Parameters(CheckSessionHealthRequest {
                session_id: "nonexistent-session-id".to_string(),
            }))
            .unwrap();
        assert!(result.contains("unknown"));
    }

    #[test]
    fn check_session_health_rejects_empty_session_id() {
        let s = AgentflareMcp::new();
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
        let s = AgentflareMcp::new();
        let result = s.list_resources_sync();
        let uris: Vec<&str> = result.resources.iter().map(|r| r.uri.as_str()).collect();
        assert_eq!(uris, vec!["agentflare://sessions", "agentflare://nudges"]);
    }

    #[test]
    fn read_resource_nudges_returns_nudges_json() {
        let s = AgentflareMcp::new();
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
        let s = AgentflareMcp::new();
        let err = s.read_resource_sync("agentflare://bogus").unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::RESOURCE_NOT_FOUND);
    }
}
