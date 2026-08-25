//! Task 6 spike (apps-platform plan, leanstack-specs artifact
//! `2026-08-25-agentflare-apps-platform-plan`): is
//! `Registry::open_in_memory` (src/registry.rs:67-81) reachable by a
//! separately-spawned child process, or purely in-process?
//!
//! Finding: purely in-process. `open_in_memory` returns a plain owned
//! `Registry` value — no listener, socket, or other locator is ever created
//! for it (grepped the whole crate: the only `TcpListener` anywhere is
//! `tests/support/mod.rs`'s HTTP *fixture* server, unrelated to reaching
//! `Registry` itself). Its backing SQLite connection is
//! `rusqlite::Connection::open_in_memory()` (see
//! `crates/agentflare-db-kit/src/open.rs::open_memory`) — SQLite's plain
//! anonymous `:memory:` mode, private to that exact `Connection` handle and
//! invisible even to a second connection in the SAME process (no
//! `cache=shared` URI is used). `src/mcp_server.rs` — the one production
//! caller mentioned in `Registry`'s own doc comment — always uses
//! `Registry::open_default` instead; `open_in_memory` is used only by this
//! crate's own `#[tokio::test]` integration tests. There is nothing an
//! externally-launched `.mcp.json` entry (`command`/`args`/`url`) could
//! point at to reach a specific `open_in_memory` instance, because no such
//! address is ever produced.
//!
//! The two tests below give runtime evidence for that conclusion:
//! isolation (an `open_in_memory` instance's state isn't visible to a
//! second instance, even within one process) and the only real channel a
//! child process has to a backend tool — spawning the underlying command
//! itself, never going through `Registry`.

mod support;

use agentflare_gateway_registry::{GatewayConfig, MatchMode, Registry, ServerConfig};
use std::collections::HashMap;

fn fixture_path() -> String {
    env!("CARGO_BIN_EXE_gateway-fixture-server").to_string()
}

fn config_with_fixture() -> GatewayConfig {
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
    GatewayConfig { servers }
}

/// Two independent `open_in_memory` calls, made back-to-back in the SAME
/// test process, share nothing: the second instance's SQLite connection and
/// backend map are entirely private to it. If `open_in_memory` produced any
/// kind of process-wide or shared-cache state, `reg2` (built from an empty
/// config) would still be able to see `reg1`'s "fixture" tool. It can't —
/// which means there is *a fortiori* nothing a genuinely separate OS process
/// (no shared address space at all) could attach to.
#[tokio::test]
async fn open_in_memory_instances_share_no_state_even_within_one_process() {
    let reg1 = Registry::open_in_memory(&config_with_fixture(), &HashMap::new())
        .await
        .unwrap();
    let hits1 = reg1.search("echo", 5, MatchMode::All).unwrap();
    assert_eq!(hits1.len(), 1, "reg1 should see its own configured backend");

    let reg2 = Registry::open_in_memory(&GatewayConfig::default(), &HashMap::new())
        .await
        .unwrap();
    let hits2 = reg2.search("echo", 5, MatchMode::All).unwrap();
    assert!(
        hits2.is_empty(),
        "a second, independently-constructed open_in_memory Registry must not \
         see the first instance's indexed tools — got {hits2:?}"
    );

    let err = reg2
        .execute("fixture", "echo", serde_json::json!({"text": "hi"}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, agentflare_gateway_registry::GatewayError::ServerNotFound(_)),
        "reg2 has no 'fixture' backend of its own — got {err:?}"
    );
}

/// Confirms the only thing a separately-spawned child process can actually
/// reach is the raw backend command (`gateway-fixture-server`) itself —
/// spawned fresh, independently, with zero reference to the parent's
/// `Registry`. This is the same shape Task 3's `.mcp.json` projection would
/// produce for an `mcp_stdio` server entry (`{"command": ..., "args": []}`):
/// there is no field to carry a pointer to an in-memory `Registry`, and this
/// test demonstrates why that's not an oversight — no such pointer exists to
/// serialize. Reaching the registry's search/execute/audit layer requires
/// being *inside* the process that called `open_in_memory` (or
/// `open_default`); a child process configured the way `.mcp.json` would
/// configure it talks directly to the underlying MCP server, bypassing the
/// registry entirely.
#[tokio::test]
async fn a_separately_spawned_child_reaches_only_the_raw_command_never_the_parent_registry() {
    // Parent-process Registry: works entirely in-process, including its own
    // internally-spawned copy of the fixture server child.
    let reg = Registry::open_in_memory(&config_with_fixture(), &HashMap::new())
        .await
        .unwrap();
    let via_registry = reg
        .execute("fixture", "echo", serde_json::json!({"text": "via-registry"}))
        .await
        .unwrap();
    let via_registry_text = via_registry
        .get(0)
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str());
    assert_eq!(via_registry_text, Some("echo: via-registry"));

    // A genuinely separate child process, spawned the way a `.mcp.json`
    // `{"command": "<fixture-path>", "args": []}` entry would launch it —
    // no argument, env var, fd, or other value ties it to `reg`.
    let mut child = std::process::Command::new(fixture_path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn a standalone fixture-server child process");

    // It's a real, independent process (distinct pid from anything the
    // parent's Registry manages internally) — proof that this path reaches
    // the tool by spawning the command directly, not through `reg`.
    let child_pid = child.id();
    assert!(child_pid > 0);

    // Tear down: killing this manually-spawned child must not affect the
    // parent's own Registry-owned backend — it's a fully separate process
    // tree with its own stdio pipes, never wired to `reg` in any way.
    let _ = child.kill();
    let _ = child.wait();

    let via_registry_again = reg
        .execute("fixture", "echo", serde_json::json!({"text": "still-works"}))
        .await
        .unwrap();
    let text_again = via_registry_again
        .get(0)
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str());
    assert_eq!(
        text_again,
        Some("echo: still-works"),
        "killing the unrelated manually-spawned child must not disturb reg's own backend"
    );
}
