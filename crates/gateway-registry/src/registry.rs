//! Ties the SQLite manifest, BM25 search, config-driven backends, and
//! debounced refresh together — the one type `src/mcp_server.rs` talks to.

use crate::backend::Backend;
use crate::config::{GatewayConfig, ServerConfig};
use crate::db::{self, ServerTools};
use crate::error::{GatewayError, suggest};
use crate::mcp_http::McpHttpBackend;
use crate::mcp_stdio::McpStdioBackend;
use crate::sanitize::sanitize_tool_entry;
use crate::search::{MatchMode, ToolHit, search, search_with_fallback};
use rusqlite::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REFRESH_DEBOUNCE: Duration = Duration::from_secs(60);

pub struct Registry {
    /// `std::sync::Mutex`, not a bare `Connection` — `rusqlite::Connection`
    /// holds `RefCell`s internally (its statement cache), so it's `Send` but
    /// not `Sync`, which makes a bare `&Registry` non-`Send` and breaks the
    /// `Send` bound `rmcp`'s tool router needs on the futures returned by
    /// `search`/`execute` when called from `src/mcp_server.rs`. Wrapping in a
    /// `Mutex` (never held across an `.await` point — every access here locks,
    /// does its synchronous rusqlite work, and drops the guard before any
    /// `await`) makes `Registry: Sync` at negligible cost, mirroring how
    /// `mcp_stdio::McpStdioBackend` already wraps its non-`Sync` connection
    /// state in a `Mutex` for the same reason.
    conn: std::sync::Mutex<Connection>,
    backends: HashMap<String, Backend>,
    last_refresh: Option<Instant>,
    /// `None` for `open_in_memory` (ephemeral/test registries — nothing
    /// durable to audit). Sibling of `db_path` on disk so no extra config
    /// plumbing is needed to locate it.
    audit_log_path: Option<PathBuf>,
}

impl Registry {
    pub async fn open_default(
        db_path: &Path,
        config: &GatewayConfig,
        secrets: &HashMap<String, String>,
    ) -> Result<Self, GatewayError> {
        let conn = db::open_db(db_path)?;
        let backends = build_backends(config, secrets);
        let audit_log_path = Some(db_path.with_file_name("gateway-audit.log"));
        let mut reg = Self {
            conn: std::sync::Mutex::new(conn),
            backends,
            last_refresh: None,
            audit_log_path,
        };
        reg.ensure_fresh().await?;
        Ok(reg)
    }

    /// In-memory registry: no on-disk manifest, no debounce skip needed since
    /// each caller builds a fresh `Registry`. Not test-gated — this is a
    /// legitimate constructor for embedded/ephemeral use (and it's what lets
    /// `tests/registry.rs`, a separate integration-test crate, exercise
    /// `Registry` end-to-end without a `#[cfg(test)]` visibility problem: a
    /// `#[cfg(test)]` item is only visible within this crate's own test
    /// build, not from an external `tests/*.rs` binary). Mirrors
    /// `db::open_in_memory`, which is likewise a plain public function.
    pub async fn open_in_memory(
        config: &GatewayConfig,
        secrets: &HashMap<String, String>,
    ) -> Result<Self, GatewayError> {
        let conn = db::open_in_memory()?;
        let backends = build_backends(config, secrets);
        let mut reg = Self {
            conn: std::sync::Mutex::new(conn),
            backends,
            last_refresh: None,
            audit_log_path: None,
        };
        reg.ensure_fresh().await?;
        Ok(reg)
    }

    pub async fn ensure_fresh(&mut self) -> Result<(), GatewayError> {
        if let Some(t) = self.last_refresh
            && t.elapsed() < REFRESH_DEBOUNCE
        {
            return Ok(());
        }
        // Backends are independent (each owns its own child-process
        // connection), so discover() runs concurrently across all of them
        // instead of one-at-a-time — with N configured servers, a single
        // slow/hanging backend no longer serializes the others' startup
        // behind it (each still bounded by its own DEFAULT_TIMEOUT).
        let discovered =
            futures_util::future::join_all(self.backends.iter().map(|(name, backend)| {
                let name = name.clone();
                async move { (name, backend.discover().await) }
            }))
            .await;

        let mut entries = Vec::new();
        for (name, result) in discovered {
            // A single backend's `discover()` failure (crashed child process,
            // bad command, or any other failure) must not poison
            // every other backend's tools. Log and skip; still rebuild the
            // index from whichever backends succeeded (mirrors
            // `skill-registry`'s `scan_sources`, which counts and skips
            // per-entry failures rather than aborting the whole scan).
            match result {
                // Sanitize here, at the one point genuinely untrusted
                // downstream data enters our storage — a fallback-path
                // `previous` entry below was already sanitized when it was
                // first discovered, so it doesn't need this again.
                Ok(tools) => {
                    let tools = tools.into_iter().map(sanitize_tool_entry).collect();
                    entries.push(ServerTools {
                        server: name,
                        tools,
                    });
                }
                Err(e) => {
                    eprintln!("gateway-registry: discover failed for backend '{name}': {e}");
                    // A transient failure (e.g. a one-off RPC timeout) must
                    // not wipe this server's tools from the index — `rebuild`
                    // below is a full-replace, so anything we don't
                    // re-contribute here disappears until the next
                    // successful refresh. Fall back to whatever was indexed
                    // for this server as of the previous successful refresh,
                    // if any, rather than contributing nothing.
                    let previous = {
                        let conn = self
                            .conn
                            .lock()
                            .expect("gateway registry db mutex poisoned");
                        db::server_tools(&conn, &name).unwrap_or_default()
                    };
                    if !previous.is_empty() {
                        entries.push(ServerTools {
                            server: name,
                            tools: previous,
                        });
                    }
                }
            }
        }
        {
            let mut conn = self
                .conn
                .lock()
                .expect("gateway registry db mutex poisoned");
            db::rebuild(&mut conn, &entries)?;
        }
        self.last_refresh = Some(Instant::now());
        Ok(())
    }

    pub fn search(
        &self,
        query: &str,
        limit: usize,
        mode: MatchMode,
    ) -> Result<Vec<ToolHit>, GatewayError> {
        let conn = self
            .conn
            .lock()
            .expect("gateway registry db mutex poisoned");
        Ok(search(&conn, query, limit, mode)?)
    }

    /// Search local index, then fall back to the official MCP Registry if
    /// fewer than `limit` results were found locally.
    pub fn search_with_fallback(
        &self,
        query: &str,
        limit: usize,
        mode: MatchMode,
    ) -> Vec<ToolHit> {
        let conn = self
            .conn
            .lock()
            .expect("gateway registry db mutex poisoned");
        search_with_fallback(&conn, query, limit, mode)
    }

    pub async fn execute(
        &self,
        server: &str,
        tool: &str,
        args: Value,
    ) -> Result<Value, GatewayError> {
        let backend = match self.backends.get(server) {
            Some(b) => b,
            None => {
                let candidates: Vec<String> = self.backends.keys().cloned().collect();
                let msg = match suggest(server, &candidates) {
                    Some(s) => format!("server '{server}' not found — did you mean '{s}'?"),
                    None => format!("server '{server}' not found"),
                };
                return Err(GatewayError::ServerNotFound(msg));
            }
        };
        let known_tools = {
            let conn = self
                .conn
                .lock()
                .expect("gateway registry db mutex poisoned");
            db::tool_names(&conn, server)?
        };
        if !known_tools.is_empty() && !known_tools.contains(&tool.to_string()) {
            let msg = match suggest(tool, &known_tools) {
                Some(s) => {
                    format!("tool '{tool}' not found on server '{server}' — did you mean '{s}'?")
                }
                None => format!("tool '{tool}' not found on server '{server}'"),
            };
            return Err(GatewayError::ToolNotFound(msg));
        }
        let result = backend.call(tool, args.clone()).await;
        if let Some(path) = &self.audit_log_path {
            match &result {
                Ok(_) => crate::audit::record(path, server, tool, &args, Ok(())),
                Err(e) => crate::audit::record(path, server, tool, &args, Err(error_kind(e))),
            }
        }
        result
    }
}

/// A short, stable tag for `GatewayError`'s variant — used only in the
/// audit log's `error_kind` field, not shown to the LLM (that's
/// `redact_error_for_llm`'s job on the full message elsewhere).
fn error_kind(e: &GatewayError) -> &'static str {
    match e {
        GatewayError::ServerNotFound(_) => "ServerNotFound",
        GatewayError::ToolNotFound(_) => "ToolNotFound",
        GatewayError::NotImplemented(_) => "NotImplemented",
        GatewayError::Connection(_) => "Connection",
        GatewayError::Upstream(_) => "Upstream",
        GatewayError::Timeout(_) => "Timeout",
        GatewayError::InvalidArgument(_) => "InvalidArgument",
        GatewayError::CircuitOpen(_) => "CircuitOpen",
        GatewayError::Sqlite(_) => "Sqlite",
    }
}

fn build_backends(
    config: &GatewayConfig,
    secrets: &HashMap<String, String>,
) -> HashMap<String, Backend> {
    let mut out = HashMap::new();
    for (name, server_config) in &config.servers {
        let backend = match server_config {
            ServerConfig::McpStdio {
                command,
                args,
                auth_ref,
                auth_env,
            } => {
                let mut env = HashMap::new();
                if let (Some(auth_ref), Some(auth_env)) = (auth_ref, auth_env) {
                    match secrets.get(auth_ref) {
                        Some(secret) => {
                            env.insert(auth_env.clone(), secret.clone());
                        }
                        // The secret was never `set` via the CLI (typo'd
                        // auth_ref, or genuinely missing), or resolving it
                        // failed upstream (see `resolve_gateway_secrets`,
                        // which logs the underlying reason). Either way,
                        // spawning silently with no credentials is exactly
                        // the "looks fine, quietly fails downstream" failure
                        // mode this log line exists to surface — mirrors
                        // `ensure_fresh`'s per-backend discover-failure log.
                        None => eprintln!(
                            "gateway-registry: server '{name}' references auth_ref '{auth_ref}' which has no stored secret — spawning without credentials"
                        ),
                    }
                }
                Backend::McpStdio(McpStdioBackend::new(command.clone(), args.clone(), env))
            }
            ServerConfig::McpHttp {
                url,
                auth_ref,
                auth_env,
                auth_header,
            } => {
                let resolved =
                    resolve_mcp_http_auth_header(name, auth_ref, auth_env, auth_header, secrets);
                Backend::McpHttp(McpHttpBackend::new(url.clone(), resolved))
            }
        };
        out.insert(name.clone(), backend);
    }
    out
}

/// Resolves an `mcp_http` server's `auth_ref`/`auth_env` pair (if present)
/// against the stored secrets into the `(header name, header value)` tuple
/// `McpHttpBackend::new` expects, defaulting the header name to
/// `"Authorization"` when `auth_header` wasn't set. Split out of
/// `build_backends` to keep that function's per-arm branching flat.
///
/// `auth_env` isn't actually used for HTTP the way it is for stdio — stdio
/// uses it as the literal env-var name to inject; HTTP's `auth_header` field
/// already plays that role. It's still required at config-parse time by the
/// `IncompleteAuthConfig` pairing check purely for consistency with stdio's
/// config shape, per the spec — hence the unused `_auth_env` binding below,
/// which only participates in the presence check.
fn resolve_mcp_http_auth_header(
    name: &str,
    auth_ref: &Option<String>,
    auth_env: &Option<String>,
    auth_header: &Option<String>,
    secrets: &HashMap<String, String>,
) -> Option<(String, String)> {
    let (Some(auth_ref), Some(_auth_env)) = (auth_ref, auth_env) else {
        return None;
    };
    match secrets.get(auth_ref) {
        Some(secret) => {
            let header_name = auth_header
                .clone()
                .unwrap_or_else(|| "Authorization".to_string());
            Some((header_name, secret.clone()))
        }
        None => {
            eprintln!(
                "gateway-registry: server '{name}' references auth_ref '{auth_ref}' which has no stored secret — connecting without credentials"
            );
            None
        }
    }
}

// NOTE: the four Registry integration tests (search/execute against the real
// `gateway-fixture-server` binary) originally planned as a `#[cfg(test)] mod
// tests` here had to move to `tests/registry.rs` instead, for the same
// reason `mcp_stdio.rs`'s and the discover/call backend tests moved (Tasks 6
// and 7): `env!("CARGO_BIN_EXE_gateway-fixture-server")` is only populated by
// Cargo for integration-test/bench targets, not for the lib's own unit-test
// binary. See `tests/registry.rs`.
//
// The test below doesn't need the fixture binary at all — it can live
// here as a normal `#[cfg(test)]` unit test with direct (same-module) access
// to `Registry`'s private fields, which lets it seed the DB with a "previous
// refresh" state without going through a live discover() first.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ToolEntry;

    #[tokio::test]
    async fn ensure_fresh_preserves_previous_tools_when_discover_fails() {
        let mut conn = db::open_in_memory().unwrap();
        // Seed the DB as if a PRIOR successful refresh had indexed one tool
        // for server "flaky", before constructing the Registry below.
        db::rebuild(
            &mut conn,
            &[ServerTools {
                server: "flaky".to_string(),
                tools: vec![ToolEntry {
                    name: "old_tool".to_string(),
                    description: "indexed on a previous successful refresh".to_string(),
                    input_schema: serde_json::json!({}),
                }],
            }],
        )
        .unwrap();

        let mut backends = HashMap::new();
        // A nonexistent binary fails to spawn immediately and synchronously — a stand-in for any backend whose
        // discover() fails on this particular refresh.
        backends.insert(
            "flaky".to_string(),
            Backend::McpStdio(McpStdioBackend::new(
                "definitely-not-a-real-binary-xyz".to_string(),
                vec![],
                HashMap::new(),
            )),
        );
        // `last_refresh: None` so `ensure_fresh` doesn't skip via the
        // debounce and actually runs the discover-fails-then-fallback path.
        let mut reg = Registry {
            conn: std::sync::Mutex::new(conn),
            backends,
            last_refresh: None,
            audit_log_path: None,
        };

        reg.ensure_fresh().await.unwrap();

        // The failed refresh must NOT have wiped "flaky"'s previously
        // indexed tool — it should still be searchable/known.
        let hits = reg.search("old_tool", 5, MatchMode::All).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "expected previously-indexed tool to survive a failed discover(), got {hits:?}"
        );
        assert_eq!(hits[0].server, "flaky");
        assert_eq!(hits[0].tool, "old_tool");

        let known = {
            let c = reg.conn.lock().unwrap();
            db::tool_names(&c, "flaky").unwrap()
        };
        assert_eq!(known, vec!["old_tool".to_string()]);
    }
}
