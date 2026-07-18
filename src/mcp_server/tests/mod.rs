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
fn validate_conventional_pr_title_accepts_known_types_rejects_others() {
    for good in [
        "feat: add thing",
        "fix(scope): bug",
        "chore!: breaking rename",
        "docs: update readme",
    ] {
        assert!(
            AgentflareMcp::validate_conventional_pr_title(good).is_ok(),
            "expected {good:?} to pass"
        );
    }
    for bad in [
        "Add thing",
        "Relicense repo from MIT to Apache-2.0",
        "Feat: wrong case",
        "unknown: not a real type",
    ] {
        assert!(
            AgentflareMcp::validate_conventional_pr_title(bad).is_err(),
            "expected {bad:?} to fail"
        );
    }
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
    let src = include_str!("../../mcp_server.rs");
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

mod action_tests;
mod artifact_tests;
mod asset_tests;
mod item_tests;
