//! BM25 search over the FTS5 tools index. Uses shared primitives from
//! `flare-search-kit` for query sanitization and limit clamping.

// Re-exports for external consumers building their own FTS5 queries.
#[allow(unused_imports)]
pub use flare_search_kit::{Bm25Weights, MatchMode};
use flare_search_kit::{clamped_limit, fts_query};
use rusqlite::Connection;
use serde_json::Value;

/// How to install a server found via the MCP Registry fallback.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallHint {
    /// Package registry type: "npm", "pypi", "oci", etc.
    pub registry_type: String,
    /// Package identifier (e.g. "@gitkraken/gk" or "githits").
    pub identifier: String,
    /// Hint about the runtime command: "npx", "uvx", "docker".
    pub runtime_hint: Option<String>,
}

/// Whether a `ToolHit` came from the local index or the remote registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum HitSource {
    Local,
    Registry,
}

/// Score assigned to every MCP-Registry fallback hit. Local hits come from
/// SQLite FTS5 bm25(), which ranks ASCENDING -- a lower, more-negative
/// number is a better match (see ORDER BY score in search() below, with no
/// DESC). This sentinel is larger than any realistic bm25 score, so
/// registry hits always sort after every local hit if the merged list is
/// ever re-sorted by score using that same ascending convention -- unlike
/// a small positive placeholder, which would sort before real (negative)
/// local scores and invert the intended ranking.
pub const REGISTRY_FALLBACK_SCORE: f64 = f64::MAX;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ToolHit {
    pub server: String,
    pub tool: String,
    pub description: String,
    pub input_schema: Value,
    pub score: f64,
    /// Where this hit was found (local index or remote registry).
    pub source: HitSource,
    /// How to install the server (only present for registry hits).
    pub install_hint: Option<InstallHint>,
    /// Streamable HTTP URL for registry hits with remotes.
    pub remote_url: Option<String>,
}

pub fn search(
    conn: &Connection,
    query: &str,
    limit: usize,
    mode: MatchMode,
) -> rusqlite::Result<Vec<ToolHit>> {
    let Some(fts) = fts_query(query, mode) else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT t.server, t.name, t.description, t.input_schema,
                bm25(tools_fts, 2.0, 3.0, 1.0) AS score
         FROM tools_fts
         JOIN tools t ON t.rowid = tools_fts.rowid
         WHERE tools_fts MATCH ?1
         ORDER BY score
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![fts, clamped_limit(limit)], |r| {
        let schema_json: String = r.get(3)?;
        let input_schema: Value = serde_json::from_str(&schema_json).unwrap_or(Value::Null);
        Ok(ToolHit {
            server: r.get(0)?,
            tool: r.get(1)?,
            description: r.get(2)?,
            input_schema,
            score: r.get(4)?,
            source: HitSource::Local,
            install_hint: None,
            remote_url: None,
        })
    })?;
    rows.collect()
}

/// Fold registry-fallback hits into an already-fetched local result set, up
/// to `limit` total. Pure/no I/O by design -- the caller fetches `registry`
/// (typically via `registry_search::search_registry`) AFTER releasing
/// whatever lock guarded the local `search()` call, so a slow or hung
/// registry request can never block other callers of the local index.
pub fn merge_registry_hits(
    mut local: Vec<ToolHit>,
    limit: usize,
    registry: Vec<crate::registry_search::RegistryHit>,
) -> Vec<ToolHit> {
    let remaining = limit.saturating_sub(local.len());
    local.extend(registry.into_iter().take(remaining).map(|hit| ToolHit {
        server: hit.server,
        tool: String::new(),
        description: hit.description,
        input_schema: Value::Null,
        score: REGISTRY_FALLBACK_SCORE,
        source: HitSource::Registry,
        install_hint: hit.install_hint,
        remote_url: hit.remote_url,
    }));
    local
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ServerTools, open_in_memory, rebuild};
    use crate::registry_search::RegistryHit;
    use crate::types::ToolEntry;

    fn registry_hit(server: &str) -> RegistryHit {
        RegistryHit {
            server: server.to_string(),
            description: "from the registry".to_string(),
            install_hint: None,
            remote_url: None,
        }
    }

    fn seed() -> Connection {
        let mut conn = open_in_memory().unwrap();
        let mk = |name: &str, desc: &str| ToolEntry {
            name: name.into(),
            description: desc.into(),
            input_schema: serde_json::json!({}),
        };
        rebuild(
            &mut conn,
            &[
                ServerTools {
                    server: "narsil".into(),
                    tools: vec![
                        mk("find_symbols", "Search for symbol definitions by pattern"),
                        mk("references", "Find all references to a symbol"),
                    ],
                },
                ServerTools {
                    server: "github".into(),
                    tools: vec![mk("list_issues", "List open issues for a repository")],
                },
            ],
        )
        .unwrap();
        conn
    }

    #[test]
    fn all_mode_requires_every_token() {
        let conn = seed();
        let hits = search(&conn, "symbol references", 5, MatchMode::All).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tool, "references");
    }

    #[test]
    fn any_mode_broadens_recall() {
        let conn = seed();
        let hits = search(&conn, "symbol issues", 5, MatchMode::Any).unwrap();
        let tools: Vec<_> = hits.iter().map(|h| h.tool.as_str()).collect();
        assert!(tools.contains(&"find_symbols"));
        assert!(tools.contains(&"references"));
        assert!(tools.contains(&"list_issues"));
    }

    #[test]
    fn server_field_is_preserved() {
        let conn = seed();
        let hits = search(&conn, "issues", 5, MatchMode::Any).unwrap();
        assert_eq!(hits[0].server, "github");
    }

    #[test]
    fn fts_operators_in_query_are_neutralized() {
        let conn = seed();
        for q in [
            "symbol\" OR \"x",
            "NEAR(a b)",
            "issues*",
            "(references)",
            "col:val",
        ] {
            search(&conn, q, 5, MatchMode::Any).unwrap();
        }
    }

    #[test]
    fn empty_query_returns_empty() {
        let conn = seed();
        assert!(search(&conn, "  ", 5, MatchMode::All).unwrap().is_empty());
    }

    #[test]
    fn search_with_a_huge_limit_does_not_panic_and_still_returns_results() {
        let conn = seed();
        let hits = search(&conn, "symbol references", usize::MAX, MatchMode::All).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn local_hits_carry_hit_source_local_and_no_install_hint() {
        let conn = seed();
        let hits = search(&conn, "symbol references", 5, MatchMode::All).unwrap();
        assert_eq!(hits[0].source, HitSource::Local);
        assert!(hits[0].install_hint.is_none());
    }

    #[test]
    fn merge_registry_hits_fills_remaining_quota_only() {
        let conn = seed();
        let local = search(&conn, "symbol references", 5, MatchMode::All).unwrap();
        assert_eq!(local.len(), 1);
        let merged = merge_registry_hits(
            local,
            3,
            vec![registry_hit("a"), registry_hit("b"), registry_hit("c")],
        );
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].source, HitSource::Local);
        assert_eq!(merged[1].source, HitSource::Registry);
        assert_eq!(merged[2].source, HitSource::Registry);
    }

    #[test]
    fn merge_registry_hits_skips_registry_when_local_already_meets_limit() {
        let conn = seed();
        let local = search(&conn, "symbol references", 5, MatchMode::All).unwrap();
        let merged = merge_registry_hits(local.clone(), local.len(), vec![registry_hit("a")]);
        assert_eq!(merged.len(), local.len());
        assert!(merged.iter().all(|h| h.source == HitSource::Local));
    }

    #[test]
    fn merge_registry_hits_scores_registry_hits_worse_than_every_local_hit() {
        let conn = seed();
        let local = search(&conn, "symbol references", 5, MatchMode::All).unwrap();
        let local_scores: Vec<f64> = local.iter().map(|h| h.score).collect();
        let merged = merge_registry_hits(local, 5, vec![registry_hit("a")]);
        let reg_hit = merged
            .iter()
            .find(|h| h.source == HitSource::Registry)
            .unwrap();
        assert!(local_scores.iter().all(|&s| s < reg_hit.score));
    }
}
