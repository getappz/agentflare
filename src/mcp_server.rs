//! Minimal MCP (Model Context Protocol) server over stdio.
//!
//! Exposes agentflare optimization state as MCP resources and tools.
//! No external dependencies — MCP is JSON-RPC 2.0 over stdin/stdout.

use crate::optimize;
use crate::optimize::Router;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

const SERVER_NAME: &str = "agentflare";
const SERVER_VERSION: &str = "1.0.0";

struct McpServer;

impl McpServer {
    fn handle(&self, method: &str, params: Option<&Value>) -> Value {
        match method {
            "initialize" => json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "resources": {},
                    "tools": {}
                },
                "serverInfo": {
                    "name": SERVER_NAME,
                    "version": SERVER_VERSION
                }
            }),
            "resources/list" => self.list_resources(),
            "resources/read" => {
                let uri = params
                    .and_then(|p| p.get("uri"))
                    .and_then(|u| u.as_str())
                    .unwrap_or("");
                self.read_resource(uri)
            }
            "tools/list" => self.list_tools(),
            "tools/call" => {
                let name = params
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let arguments = params.and_then(|p| p.get("arguments"));
                self.call_tool(name, arguments)
            }
            _ => json!({
                "error": {
                    "code": -32601,
                    "message": format!("Method not found: {method}")
                }
            }),
        }
    }

    fn list_resources(&self) -> Value {
        let runtime = optimize::load_runtime();

        json!({
            "resources": [
                {
                    "uri": "agentflare://sessions",
                    "name": "Active sessions",
                    "description": format!("{} tracked sessions", runtime.sessions.len()),
                    "mimeType": "application/json"
                },
                {
                    "uri": "agentflare://nudges",
                    "name": "Optimization nudges",
                    "description": "All nudge types agentflare can emit",
                    "mimeType": "application/json"
                }
            ]
        })
    }

    fn read_resource(&self, uri: &str) -> Value {
        match uri {
            "agentflare://sessions" => {
                let runtime = optimize::load_runtime();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let sessions: Vec<Value> = runtime
                    .sessions
                    .iter()
                    .map(|(id, record)| {
                        let elapsed_secs = now.saturating_sub(record.start_ts);
                        let hygiene =
                            optimize::session_hygiene_nudge(record, now);
                        json!({
                            "session_id": id,
                            "turn_count": record.turn_count,
                            "age_seconds": elapsed_secs,
                            "age_hours": elapsed_secs / 3600,
                            "recent_tool_calls": record.recent_tool_calls.iter().map(|c| json!({
                                "name": c.name,
                                "ts": c.ts,
                            })).collect::<Vec<_>>(),
                            "hygiene_status": if hygiene.is_some() { "stale" } else { "healthy" },
                            "hygiene_nudge": hygiene,
                        })
                    })
                    .collect();
                json!({
                    "contents": [{
                        "uri": uri,
                        "mimeType": "application/json",
                        "text": serde_json::to_string_pretty(&sessions).unwrap_or_default(),
                    }]
                })
            }
            "agentflare://nudges" => json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "application/json",
                    "text": serde_json::to_string_pretty(&json!({
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
                }]
            }),
            _ => json!({
                "contents": [{
                    "uri": uri,
                    "mimeType": "text/plain",
                    "text": format!("Unknown resource: {uri}"),
                }]
            }),
        }
    }

    fn list_tools(&self) -> Value {
        json!({
            "tools": [
                {
                    "name": "check_session_health",
                    "description": "Check if a session should be refreshed based on turn count and elapsed time.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "session_id": {
                                "type": "string",
                                "description": "The session ID to check"
                            }
                        },
                        "required": ["session_id"]
                    }
                },
                {
                    "name": "get_routing_suggestion",
                    "description": "Get a model routing suggestion for a given prompt.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "prompt": {
                                "type": "string",
                                "description": "The user's prompt to analyze"
                            }
                        },
                        "required": ["prompt"]
                    }
                }
            ]
        })
    }

    fn call_tool(&self, name: &str, arguments: Option<&Value>) -> Value {
        match name {
            "check_session_health" => {
                let sid = arguments
                    .and_then(|a| a.get("session_id"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                if sid.is_empty() {
                    return json!({
                        "content": [{"type": "text", "text": "session_id is required"}],
                        "isError": true,
                    });
                }
                let runtime = optimize::load_runtime();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let result = match runtime.sessions.get(sid) {
                    Some(record) => {
                        match optimize::session_hygiene_nudge(record, now) {
                            Some(nudge) => {
                                json!({"session_id": sid, "status": "stale", "nudge": nudge})
                            }
                            None => {
                                json!({"session_id": sid, "status": "healthy"})
                            }
                        }
                    }
                    None => json!({"session_id": sid, "status": "unknown", "message": "Session not tracked"}),
                };
                json!({
                    "content": [{"type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_default()}],
                })
            }
            "get_routing_suggestion" => {
                let prompt = arguments
                    .and_then(|a| a.get("prompt"))
                    .and_then(|p| p.as_str())
                    .unwrap_or("");
                let ctx = optimize::RouteContext {
                    prompt: prompt.to_string(),
                    session_id: String::new(),
                    turn_count: 0,
                    recent_tool_calls: vec![],
                    current_model: None,
                };
                let router = optimize::KeywordRouter;
                let result = match router.route(&ctx) {
                    Some(nudge) => json!({"suggestion": nudge}),
                    None => json!({"suggestion": null}),
                };
                json!({
                    "content": [{"type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_default()}],
                })
            }
            _ => json!({
                "content": [{"type": "text", "text": format!("Unknown tool: {name}")}],
                "isError": true,
            }),
        }
    }
}

const MAX_LINE_BYTES: usize = 1_000_000; // generous for one JSON-RPC request; bounds unterminated-input memory growth

pub fn run() {
    let server = McpServer;
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());

    loop {
        let mut raw = Vec::new();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break, // unrecoverable stdin I/O error
        }

        if raw.len() > MAX_LINE_BYTES {
            let err = json!({
                "jsonrpc": "2.0",
                "error": {"code": -32700, "message": "Parse error: request line too large"},
                "id": Value::Null,
            });
            let _ = writeln!(std::io::stdout(), "{err}");
            continue;
        }

        let line = match String::from_utf8(raw) {
            Ok(s) => s,
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": format!("Parse error: invalid UTF-8: {e}")},
                    "id": Value::Null,
                });
                let _ = writeln!(std::io::stdout(), "{err}");
                continue;
            }
        };

        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err = json!({
                    "jsonrpc": "2.0",
                    "error": {"code": -32700, "message": format!("Parse error: {e}")},
                    "id": Value::Null,
                });
                let _ = writeln!(std::io::stdout(), "{err}");
                continue;
            }
        };

        let method = request.get("method").and_then(|m| m.as_str());
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let params = request.get("params");

        if matches!(method, Some("notifications/initialized")) {
            continue;
        }

        let result = match method {
            Some(m) => server.handle(m, params),
            None => json!({"error": {"code": -32600, "message": "Invalid request: no method"}}),
        };

        let response = if result.get("error").is_some() {
            json!({"jsonrpc": "2.0", "id": id, "error": result["error"]})
        } else {
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        };

        let _ = writeln!(std::io::stdout(), "{response}");
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_resources_has_sessions_and_nudges() {
        let s = McpServer;
        let result = s.list_resources();
        let resources = result["resources"].as_array().unwrap();
        let uris: Vec<&str> = resources
            .iter()
            .map(|r| r["uri"].as_str().unwrap())
            .collect();
        assert!(uris.contains(&"agentflare://sessions"));
        assert!(uris.contains(&"agentflare://nudges"));
    }

    #[test]
    fn list_tools_has_health_and_routing() {
        let s = McpServer;
        let result = s.list_tools();
        let tools = result["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"check_session_health"));
        assert!(names.contains(&"get_routing_suggestion"));
    }

    #[test]
    fn initialize_returns_capabilities() {
        let s = McpServer;
        let result = s.handle("initialize", None);
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["capabilities"]["resources"].is_object());
        assert!(result["capabilities"]["tools"].is_object());
    }

    #[test]
    fn unknown_method_returns_error() {
        let s = McpServer;
        let result = s.handle("nonexistent", None);
        assert!(result["error"]["code"] == -32601);
    }

    #[test]
    fn check_session_health_unknown_returns_status() {
        let s = McpServer;
        let args = json!({"session_id": "nonexistent-session-id"});
        let result = s.call_tool("check_session_health", Some(&args));
        let content = result["content"][0]["text"].as_str().unwrap();
        assert!(content.contains("unknown"));
    }

    #[test]
    fn routing_suggestion_returns_null_for_non_locate() {
        let s = McpServer;
        let args = json!({"prompt": "refactor the payment module"});
        let result = s.call_tool("get_routing_suggestion", Some(&args));
        let content = result["content"][0]["text"].as_str().unwrap();
        assert!(content.contains("null"));
    }

    #[test]
    fn routing_suggestion_returns_nudge_for_find() {
        let s = McpServer;
        let args = json!({"prompt": "find the auth handler"});
        let result = s.call_tool("get_routing_suggestion", Some(&args));
        let content = result["content"][0]["text"].as_str().unwrap();
        assert!(content.contains("cheap-model"));
    }
}
