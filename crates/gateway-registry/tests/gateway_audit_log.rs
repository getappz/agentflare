//! Proves `Registry::open_default` (unlike `open_in_memory`, used by most of
//! this crate's other integration tests) writes an audit-log line for every
//! `execute()` call, as a sibling file next to the SQLite manifest — and
//! that raw args never appear in it, only a hash.

use agentflare_gateway_registry::{GatewayConfig, Registry, ServerConfig};
use serde_json::Value;
use std::collections::HashMap;

fn fixture_path() -> String {
    env!("CARGO_BIN_EXE_gateway-fixture-server").to_string()
}

#[tokio::test]
async fn execute_appends_an_audit_log_entry_without_leaking_raw_args() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("gateway.db");
    let audit_path = tmp.path().join("gateway-audit.log");

    let mut servers = HashMap::new();
    servers.insert(
        "fixture".to_string(),
        ServerConfig::McpStdio {
            command: fixture_path(),
            args: vec![],
            auth_ref: None,
            auth_env: None,
        },
    );
    let config = GatewayConfig { servers };

    let reg = Registry::open_default(&db_path, &config, &HashMap::new())
        .await
        .unwrap();
    assert!(
        !audit_path.exists(),
        "no calls yet — audit log shouldn't exist before the first execute()"
    );

    reg.execute(
        "fixture",
        "echo",
        serde_json::json!({"text": "top-secret-value"}),
    )
    .await
    .unwrap();
    // "fail" is a real, discovered tool (added to the fixture for the
    // reconnect tests) that always errors — unlike an unknown tool name,
    // this actually reaches `backend.call()` instead of being rejected by
    // `execute()`'s own pre-flight ToolNotFound check, so it exercises the
    // audit log's error path.
    reg.execute("fixture", "fail", serde_json::json!({}))
        .await
        .unwrap_err();

    let contents = std::fs::read_to_string(&audit_path).unwrap();
    assert!(
        !contents.contains("top-secret-value"),
        "raw args must never appear in the audit log: {contents}"
    );

    let lines: Vec<Value> = contents
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);

    assert_eq!(lines[0]["server"], "fixture");
    assert_eq!(lines[0]["tool"], "echo");
    assert_eq!(lines[0]["outcome"], "ok");
    assert!(lines[0]["args_hash"].as_str().unwrap().len() == 64);

    assert_eq!(lines[1]["outcome"], "err");
    assert_eq!(lines[1]["error_kind"], "Upstream");
}
